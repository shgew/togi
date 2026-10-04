package watch

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"syscall"
	"time"

	"github.com/charmbracelet/colorprofile"
	"golang.org/x/sys/unix"
	"golang.org/x/term"
)

// source reloads the journal only when events.jsonl is a different file or changed size or modification time since
// the last read.
type source struct {
	dir  string
	info os.FileInfo
	snap Snapshot
}

func (s *source) snapshot() Snapshot {
	info, err := os.Stat(filepath.Join(s.dir, "events.jsonl"))
	if err != nil {
		s.info, s.snap = nil, Load(s.dir)
		return s.snap
	}
	if s.info != nil && os.SameFile(s.info, info) && info.Size() == s.info.Size() && info.ModTime().Equal(s.info.ModTime()) {
		return s.snap
	}
	s.info, s.snap = info, Load(s.dir)
	return s.snap
}

func (s *source) frame(sc Screen) Drawn {
	return RenderView(s.snapshot(), sc, time.Now())
}

// profile is the colour profile of out; NO_COLOR with any value turns colour off, as no-color.org defines it.
func profile(out *os.File) colorprofile.Profile {
	p := colorprofile.Detect(out, os.Environ())
	if os.Getenv("NO_COLOR") != "" {
		p = min(p, colorprofile.ASCII)
	}
	return p
}

// consolePalette is the dashboard's shade for each of the 16 colour slots on the Linux console, which allows
// redefining them (ESC ] P nrrggbb). Its default grey and bold handling are too dim at low monitor brightness.
var consolePalette = [16]string{
	"000000", "aa3333", "33aa55", "aa7722", "2a4a80", "8a4aa0", "2a8a8a", "e4e4e4",
	"8b919b", "ff6b6b", "6be08a", "ffc04d", "7fb4ff", "d79bff", "5fe3e3", "ffffff",
}

func paletteSet() string {
	var b strings.Builder
	for i, rgb := range consolePalette {
		fmt.Fprintf(&b, "\x1b]P%X%s", i, rgb)
	}
	return b.String()
}

const paletteReset = "\x1b]R"

// Run redraws the dashboard for the journal in dir on out once a second until ctx ends. With in a terminal, keys
// switch between the main view, help and the event log, and scroll the help and the log.
func Run(ctx context.Context, dir string, out, in *os.File) error {
	src := source{dir: dir}
	tick := time.NewTicker(time.Second)
	defer tick.Stop()
	return Show(ctx, out, in, tick.C, src.frame)
}

// Show runs the redraw loop on out, drawing frame whenever tick fires, the terminal is resized or a key changes what
// it shows, until ctx ends or the viewer quits.
func Show(ctx context.Context, out, in *os.File, tick <-chan time.Time, frame Frame) error {
	winch := make(chan os.Signal, 1)
	signal.Notify(winch, syscall.SIGWINCH)
	defer signal.Stop(winch)
	o := options{palette: os.Getenv("TERM") == "linux"}
	if in != nil && term.IsTerminal(int(in.Fd())) {
		state, err := term.MakeRaw(int(in.Fd()))
		if err != nil {
			return fmt.Errorf("read keys: %w", err)
		}
		defer func() { _ = term.Restore(int(in.Fd()), state) }()
		keys, stop, err := readKeys(ctx, in, unix.Poll)
		if err != nil {
			return fmt.Errorf("read keys: %w", err)
		}
		o.keys, o.stopKeys = keys, stop
	}
	return show(ctx, out, func() (int, int, error) { return term.GetSize(int(out.Fd())) }, tick, winch, profile(out), frame, o)
}

type options struct {
	palette  bool
	keys     <-chan key
	stopKeys func()
}

// key is one key press: a printable or control character, or one of the named keys below.
type key string

const (
	keyEsc      key = "esc"
	keyUp       key = "up"
	keyDown     key = "down"
	keyPageUp   key = "pgup"
	keyPageDown key = "pgdn"
	keyHome     key = "home"
	keyEnd      key = "end"
)

// csiKeys names the escape sequences the Linux console and common terminals send for the keys that scroll.
var csiKeys = map[string]key{
	"[A": keyUp, "OA": keyUp, "[B": keyDown, "OB": keyDown,
	"[5~": keyPageUp, "[6~": keyPageDown,
	"[H": keyHome, "OH": keyHome, "[1~": keyHome, "[7~": keyHome,
	"[F": keyEnd, "OF": keyEnd, "[4~": keyEnd, "[8~": keyEnd,
}

// decodeKeys decodes a complete batch; the stream reader retains unfinished escape sequences.
func decodeKeys(b []byte) []key {
	keys, _ := splitKeys(b, true)
	return keys
}

func splitKeys(b []byte, final bool) ([]key, int) {
	var keys []key
	for i := 0; i < len(b); {
		if b[i] != 0x1b {
			keys = append(keys, key(b[i:i+1]))
			i++
			continue
		}
		if i+1 == len(b) {
			if !final {
				return keys, i
			}
			keys = append(keys, keyEsc)
			i++
			continue
		}
		if b[i+1] != '[' && b[i+1] != 'O' {
			keys = append(keys, keyEsc)
			i++
			continue
		}
		j := i + 2
		for j < len(b) && (b[j] < 0x40 || b[j] > 0x7e) {
			j++
		}
		if j == len(b) {
			if !final {
				return keys, i
			}
			return keys, len(b)
		}
		if k, ok := csiKeys[string(b[i+1:j+1])]; ok {
			keys = append(keys, k)
		}
		i = j + 1
	}
	return keys, len(b)
}

const escapeTimeout = 50 * time.Millisecond

func sendKeys(ctx context.Context, keys chan<- key, batch []key) bool {
	for _, k := range batch {
		select {
		case keys <- k:
		case <-ctx.Done():
			return false
		}
	}
	return true
}

// readKeys polls a private wake pipe alongside input so stopping never closes or changes the caller's file.
// stop cancels and joins both the reader and its wake callback before releasing their descriptors.
func readKeys(ctx context.Context, in *os.File, poll func([]unix.PollFd, int) (int, error)) (<-chan key, func(), error) {
	wake, notify, err := os.Pipe()
	if err != nil {
		return nil, nil, err
	}
	ctx, cancel := context.WithCancel(ctx)
	woken := make(chan struct{})
	stopWake := context.AfterFunc(ctx, func() {
		_, _ = notify.Write([]byte{1})
		close(woken)
	})
	keys, done := make(chan key), make(chan struct{})
	go func() {
		defer close(done)
		defer close(keys)
		fds := []unix.PollFd{
			{Fd: int32(in.Fd()), Events: unix.POLLIN},
			{Fd: int32(wake.Fd()), Events: unix.POLLIN},
		}
		var buf [128]byte
		used := 0
		var deadline time.Time
		for ctx.Err() == nil {
			timeout := -1
			if used > 0 {
				timeout = int((max(time.Until(deadline), 0) + time.Millisecond - 1) / time.Millisecond)
			}
			n, err := poll(fds, timeout)
			if errors.Is(err, unix.EINTR) {
				continue
			}
			if err != nil || ctx.Err() != nil || fds[1].Revents != 0 {
				return
			}
			if n == 0 {
				if !sendKeys(ctx, keys, decodeKeys(buf[:used])) {
					return
				}
				used = 0
				continue
			}
			if fds[0].Revents&unix.POLLNVAL != 0 {
				return
			}
			if fds[0].Revents&(unix.POLLIN|unix.POLLHUP|unix.POLLERR) == 0 {
				continue
			}
			n, err = unix.Read(int(in.Fd()), buf[used:])
			if errors.Is(err, unix.EINTR) || errors.Is(err, unix.EAGAIN) {
				continue
			}
			if err != nil {
				return
			}
			if n == 0 {
				sendKeys(ctx, keys, decodeKeys(buf[:used]))
				return
			}
			batch, consumed := splitKeys(buf[:used+n], false)
			if !sendKeys(ctx, keys, batch) {
				return
			}
			if used == 0 || consumed > 0 {
				deadline = time.Now().Add(escapeTimeout)
			}
			used = copy(buf[:], buf[consumed:used+n])
			if used == len(buf) {
				// An oversized unsupported sequence must not fill the read buffer indefinitely.
				used = 0
			}
		}
	}()
	stop := func() {
		cancel()
		<-done
		if !stopWake() {
			<-woken
		}
		_ = wake.Close()
		_ = notify.Close()
	}
	return keys, stop, nil
}

func run(ctx context.Context, dir string, out io.Writer, size func() (int, int, error), tick <-chan time.Time, winch <-chan os.Signal, p colorprofile.Profile) error {
	src := source{dir: dir}
	return show(ctx, out, size, tick, winch, p, src.frame, options{})
}

func show(ctx context.Context, out io.Writer, size func() (int, int, error), tick <-chan time.Time, winch <-chan os.Signal, p colorprofile.Profile, frame Frame, o options) error {
	if o.stopKeys != nil {
		defer o.stopKeys()
	}
	enter, leave := "\x1b[?25l\x1b[2J", "\x1b[0m\x1b[2J\x1b[H\x1b[?25h"
	if o.palette {
		enter, leave = paletteSet()+enter, leave+paletteReset
	}
	if _, err := fmt.Fprint(out, enter); err != nil {
		return fmt.Errorf("clear terminal: %w", err)
	}
	defer fmt.Fprint(out, leave)

	var buf bytes.Buffer
	styled := &colorprofile.Writer{Forward: &buf, Profile: p}
	var lastW, lastH int
	sc := Screen{View: MainView, Keys: o.keys != nil}
	for {
		w, h, err := size()
		if err != nil {
			return fmt.Errorf("read terminal size: %w", err)
		}
		buf.Reset()
		if w != lastW || h != lastH {
			buf.WriteString("\x1b[2J")
			lastW, lastH = w, h
		}
		buf.WriteString("\x1b[H")
		sc.Width, sc.Height = w, h
		d := frame(sc)
		for i, line := range d.Lines {
			if i > 0 {
				buf.WriteString("\r\n")
			}
			_, _ = styled.Write([]byte(line))
			buf.WriteString("\x1b[K")
		}
		buf.WriteString("\x1b[J")
		if _, err := out.Write(buf.Bytes()); err != nil {
			return fmt.Errorf("draw frame: %w", err)
		}
		select {
		case <-ctx.Done():
			return nil
		case <-tick:
		case <-winch:
		case k, ok := <-o.keys:
			if !ok {
				return nil
			}
			next, quit := press(sc, k, d.Scroll)
			if quit {
				return nil
			}
			if next.View != sc.View {
				lastW = 0
			}
			sc = next
		}
	}
}

// press applies a key to the screen: ? and L toggle the help and the event log, Esc returns to the main view, the
// arrow, page, Home and End keys (or k, j, b, space, g and G) scroll a view that can scroll up to scrolled lines, and
// q, Ctrl-C and Ctrl-D quit. The help opens at its top and the log at its end, which keeps following new events.
func press(sc Screen, k key, scrolled int) (Screen, bool) {
	page := max(sc.Height-8, 1)
	at := sc.Scroll
	if at < 0 || at > scrolled {
		at = scrolled
	}
	to := func(line int) (Screen, bool) {
		if sc.View == MainView {
			return sc, false
		}
		sc.Scroll = min(max(line, 0), scrolled)
		if sc.Scroll == scrolled && sc.View == LogView {
			sc.Scroll = -1
		}
		return sc, false
	}
	switch k {
	case "q", "Q", "\x03", "\x04":
		return sc, true
	case "?", "h", "H":
		if sc.View == HelpView {
			return Screen{View: MainView, Keys: sc.Keys}, false
		}
		return Screen{View: HelpView, Keys: sc.Keys}, false
	case "l", "L":
		if sc.View == LogView {
			return Screen{View: MainView, Keys: sc.Keys}, false
		}
		return Screen{View: LogView, Keys: sc.Keys, Scroll: -1}, false
	case keyEsc:
		return Screen{View: MainView, Keys: sc.Keys}, false
	case keyUp, "k":
		return to(at - 1)
	case keyDown, "j":
		return to(at + 1)
	case keyPageUp, "b":
		return to(at - page)
	case keyPageDown, " ", "f":
		return to(at + page)
	case keyHome, "g":
		return to(0)
	case keyEnd, "G":
		return to(scrolled)
	}
	return sc, false
}
