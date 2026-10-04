package watch

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"testing/synctest"
	"time"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/colorprofile"
	"github.com/charmbracelet/x/ansi"
	"github.com/google/go-cmp/cmp"
	"golang.org/x/sys/unix"
)

const terminalEnter = "\x1b[?25l\x1b[2J"
const terminalLeave = "\x1b[0m\x1b[2J\x1b[H\x1b[?25h"

type terminalOutput struct {
	writes   []string
	attempts int
	failAt   int
	err      error
}

func (o *terminalOutput) Write(p []byte) (int, error) {
	o.attempts++
	if o.attempts == o.failAt {
		return 0, o.err
	}
	o.writes = append(o.writes, string(p))
	return len(p), nil
}

func terminalFrame(t *testing.T, out *terminalOutput, index, width, height int, clear bool) string {
	t.Helper()
	if len(out.writes) != index+1 {
		t.Fatalf("redraw did not produce exactly one frame: writes=%d, want %d", len(out.writes), index+1)
	}
	frame := out.writes[index]
	prefix := "\x1b[H"
	if clear {
		prefix = "\x1b[2J" + prefix
	}
	if !strings.HasPrefix(frame, prefix) || !strings.HasSuffix(frame, "\x1b[J") {
		t.Fatalf("frame did not home/clear/erase as required: %q", frame)
	}
	if !clear && strings.Contains(frame, "\x1b[2J") {
		t.Fatal("unchanged terminal geometry cleared the screen")
	}
	lines := strings.Split(strings.TrimSuffix(strings.TrimPrefix(frame, prefix), "\x1b[J"), "\r\n")
	if len(lines) != height-1 {
		t.Fatalf("frame did not use requested terminal height: rows=%d want %d", len(lines), height-1)
	}
	for _, line := range lines {
		if !strings.HasSuffix(line, "\x1b[K") || lipgloss.Width(ansi.Strip(line)) > width-1 {
			t.Fatalf("frame wrote reserved column or failed to erase stale row: %q", line)
		}
	}
	return ansi.Strip(frame)
}

func TestRunRedrawResizeAndCancel(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		dir := t.TempDir()
		path := filepath.Join(dir, "events.jsonl")
		if err := os.WriteFile(path, watchSessionLine(t, "initial live frame"), 0644); err != nil {
			t.Fatal(err)
		}
		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()
		ticker := time.NewTicker(time.Second)
		defer ticker.Stop()
		winch := make(chan os.Signal, 1)
		out := &terminalOutput{}
		w, h := 160, 40
		done := make(chan error, 1)
		go func() {
			done <- run(ctx, dir, out, func() (int, int, error) { return w, h, nil }, ticker.C, winch, colorprofile.ASCII)
		}()
		synctest.Wait()
		if out.writes[0] != terminalEnter || !strings.Contains(terminalFrame(t, out, 1, w, h, true), "initial live frame") {
			t.Fatalf("terminal did not initialize and draw current journal: %q", out.writes)
		}

		data := append(watchSessionLine(t, "initial live frame"), watchWarningLine(t, "periodic live reload")...)
		if err := os.WriteFile(path, data, 0644); err != nil {
			t.Fatal(err)
		}
		time.Sleep(time.Second)
		synctest.Wait()
		if !strings.Contains(terminalFrame(t, out, 2, w, h, false), "periodic live reload") {
			t.Fatal("one-second ticker did not reload the live journal")
		}
		winch <- syscall.SIGWINCH
		synctest.Wait()
		terminalFrame(t, out, 3, w, h, false)
		w, h = 42, 8
		winch <- syscall.SIGWINCH
		synctest.Wait()
		terminalFrame(t, out, 4, w, h, true)
		cancel()
		if err := <-done; err != nil {
			t.Fatal(err)
		}
		if len(out.writes) != 6 || out.writes[5] != terminalLeave {
			t.Fatalf("cancel did not restore cursor and clear the terminal exactly once: %q", out.writes)
		}
	})
}

func TestRunRestoresCursorOnErrors(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name      string
		writeFail int
		sizeFail  int
		redraw    bool
		operation string
		restore   bool
	}{
		{name: "initial clear", writeFail: 1, operation: "clear terminal"},
		{name: "initial size", sizeFail: 1, operation: "read terminal size", restore: true},
		{name: "initial frame", writeFail: 2, operation: "draw frame", restore: true},
		{name: "redraw size", sizeFail: 2, redraw: true, operation: "read terminal size", restore: true},
		{name: "redraw frame", writeFail: 3, redraw: true, operation: "draw frame", restore: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				dir := t.TempDir()
				ctx, cancel := context.WithCancel(context.Background())
				defer cancel()
				failure := errors.New("terminal unavailable")
				out := &terminalOutput{failAt: tc.writeFail, err: failure}
				calls := 0
				size := func() (int, int, error) {
					calls++
					if calls == tc.sizeFail {
						return 0, 0, failure
					}
					return 80, 12, nil
				}
				winch := make(chan os.Signal, 1)
				done := make(chan error, 1)
				go func() { done <- run(ctx, dir, out, size, nil, winch, colorprofile.ASCII) }()
				synctest.Wait()
				if tc.redraw {
					terminalFrame(t, out, 1, 80, 12, true)
					winch <- syscall.SIGWINCH
				}
				if err := <-done; !errors.Is(err, failure) || !strings.Contains(err.Error(), tc.operation) {
					t.Fatalf("terminal failure did not retain operation and cause: %v", err)
				}
				if tc.restore {
					if out.writes[0] != terminalEnter || out.writes[len(out.writes)-1] != terminalLeave {
						t.Fatalf("initialized terminal did not restore cursor on error: %q", out.writes)
					}
				} else if len(out.writes) != 0 || out.attempts != 1 {
					t.Fatalf("failed initial clear claimed terminal initialization: writes=%q attempts=%d", out.writes, out.attempts)
				}
			})
		})
	}
}

func TestRunShowsJournalProblemAndRecovery(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		dir := t.TempDir()
		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()
		out := &terminalOutput{}
		winch := make(chan os.Signal, 1)
		done := make(chan error, 1)
		go func() {
			done <- run(ctx, dir, out, func() (int, int, error) { return 160, 40, nil }, nil, winch, colorprofile.ASCII)
		}()
		synctest.Wait()
		if !strings.Contains(terminalFrame(t, out, 1, 160, 40, true), "no session yet") {
			t.Fatal("missing journal was not displayed")
		}
		path := filepath.Join(dir, "events.jsonl")
		if err := os.WriteFile(path, []byte("not json\n"), 0644); err != nil {
			t.Fatal(err)
		}
		winch <- syscall.SIGWINCH
		synctest.Wait()
		if frame := terminalFrame(t, out, 2, 160, 40, false); !strings.Contains(frame, "invalid character") {
			t.Fatalf("live journal read error was not displayed: %s", frame)
		}
		select {
		case err := <-done:
			t.Fatalf("journal problem stopped live watch: %v", err)
		default:
		}
		if err := os.WriteFile(path, watchSessionLine(t, "live journal recovered"), 0644); err != nil {
			t.Fatal(err)
		}
		winch <- syscall.SIGWINCH
		synctest.Wait()
		frame := terminalFrame(t, out, 3, 160, 40, false)
		if !strings.Contains(frame, "live journal recovered") || strings.Contains(frame, "invalid character") {
			t.Fatalf("live watch did not replace problem with recovered session: %s", frame)
		}
		cancel()
		if err := <-done; err != nil || out.writes[len(out.writes)-1] != terminalLeave {
			t.Fatalf("recovered watch did not stop cleanly: err=%v writes=%q", err, out.writes)
		}
	})
}

func TestShowKeyboardDispatch(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()
		keys := make(chan key)
		tick := make(chan time.Time)
		out := &terminalOutput{}
		var screens []Screen
		scrolled := 20
		frame := func(sc Screen) Drawn {
			screens = append(screens, sc)
			return Drawn{Lines: []string{fmt.Sprintf("view %d scroll %d", sc.View, sc.Scroll)}, Scroll: scrolled}
		}
		done := make(chan error, 1)
		go func() {
			done <- show(ctx, out, func() (int, int, error) { return 80, 12, nil }, tick, nil, colorprofile.ASCII, frame, options{keys: keys})
		}()
		check := func(view View, scroll int, clear bool) {
			t.Helper()
			synctest.Wait()
			want := Screen{View: view, Scroll: scroll, Width: 80, Height: 12, Keys: true}
			if diff := cmp.Diff(want, screens[len(screens)-1]); diff != "" {
				t.Fatalf("consumer frame (-want +got):\n%s", diff)
			}
			text := out.writes[len(out.writes)-1]
			if strings.HasPrefix(text, "\x1b[2J\x1b[H") != clear {
				t.Fatalf("view change clear=%t: %q", clear, text)
			}
			if !strings.Contains(text, fmt.Sprintf("view %d scroll %d", view, scroll)) {
				t.Fatalf("consumer frame not drawn: %q", text)
			}
		}
		check(MainView, 0, true)
		for _, step := range []struct {
			key    key
			view   View
			scroll int
			clear  bool
		}{
			{"?", HelpView, 0, true},
			{keyDown, HelpView, 1, false},
			{keyPageDown, HelpView, 5, false},
			{keyHome, HelpView, 0, false},
			{keyEnd, HelpView, 20, false},
			{"?", MainView, 0, true},
			{keyDown, MainView, 0, false},
			{"L", LogView, -1, true},
			{keyUp, LogView, 19, false},
			{keyDown, LogView, -1, false},
			{keyUp, LogView, 19, false},
		} {
			keys <- step.key
			check(step.view, step.scroll, step.clear)
		}
		scrolled = 25
		tick <- time.Time{}
		check(LogView, 19, false)
		keys <- keyEnd
		check(LogView, -1, false)
		scrolled = 30
		tick <- time.Time{}
		check(LogView, -1, false)
		keys <- keyEsc
		check(MainView, 0, true)
		keys <- "h"
		check(HelpView, 0, true)
		keys <- "l"
		check(LogView, -1, true)
		keys <- "l"
		check(MainView, 0, true)
		frames := len(screens)
		keys <- "q"
		if err := <-done; err != nil {
			t.Fatal(err)
		}
		if len(screens) != frames || out.writes[len(out.writes)-1] != terminalLeave {
			t.Fatalf("quit redrew or failed cleanup: screens=%v writes=%q", screens, out.writes)
		}
	})
}

// These tests use real pipe reads; only poll's timeout is injected. A poll call acknowlprobes
// that the previous read has been decoded, so fragments cannot accidentally coalesce.
func TestReadKeysFragmentedSequences(t *testing.T) {
	t.Parallel()
	for _, sequence := range []struct {
		bytes string
		want  key
	}{
		{"\x1b[A", keyUp}, {"\x1b[B", keyDown},
		{"\x1bOA", keyUp}, {"\x1bOB", keyDown},
		{"\x1b[5~", keyPageUp}, {"\x1b[6~", keyPageDown},
		{"\x1b[H", keyHome}, {"\x1bOF", keyEnd},
	} {
		for split := 1; split < len(sequence.bytes); split++ {
			t.Run(fmt.Sprintf("%q/%d", sequence.bytes, split), func(t *testing.T) {
				in, writer, err := os.Pipe()
				if err != nil {
					t.Fatal(err)
				}
				defer in.Close()
				defer writer.Close()
				calls := make(chan int)
				poll := func(fds []unix.PollFd, timeout int) (int, error) {
					calls <- timeout
					return pollThroughSignals(fds)
				}
				keys, stop, err := readKeys(context.Background(), in, poll)
				if err != nil {
					t.Fatal(err)
				}
				if timeout := <-calls; timeout != -1 {
					t.Fatalf("idle poll timeout=%d", timeout)
				}
				if _, err := io.WriteString(writer, sequence.bytes[:split]); err != nil {
					t.Fatal(err)
				}
				if timeout := <-calls; timeout < 0 || timeout > int(escapeTimeout/time.Millisecond) {
					t.Fatalf("prefix did not schedule bounded timeout: %d", timeout)
				}
				select {
				case k := <-keys:
					t.Fatalf("fragment became premature key %q", k)
				default:
				}
				if _, err := io.WriteString(writer, sequence.bytes[split:]); err != nil {
					t.Fatal(err)
				}
				if got := <-keys; got != sequence.want {
					t.Fatalf("split sequence=%q want %q", got, sequence.want)
				}
				<-calls
				stop()
				if _, ok := <-keys; ok {
					t.Fatal("stopped reader left key channel open")
				}
			})
		}
	}
}

func TestReadKeysEscapeTimeoutAndEOF(t *testing.T) {
	t.Parallel()
	for _, prefix := range []string{"\x1b", "\x1b[", "\x1bO"} {
		t.Run(fmt.Sprintf("%q", prefix), func(t *testing.T) {
			in, writer, err := os.Pipe()
			if err != nil {
				t.Fatal(err)
			}
			defer in.Close()
			defer writer.Close()
			calls := make(chan int)
			poll := func(fds []unix.PollFd, timeout int) (int, error) {
				calls <- timeout
				if timeout >= 0 {
					return 0, nil
				}
				return pollThroughSignals(fds)
			}
			keys, stop, err := readKeys(context.Background(), in, poll)
			if err != nil {
				t.Fatal(err)
			}
			<-calls
			if _, err := io.WriteString(writer, prefix); err != nil {
				t.Fatal(err)
			}
			if timeout := <-calls; timeout < 0 || timeout > 50 {
				t.Fatalf("escape timeout=%d", timeout)
			}
			if prefix == "\x1b" {
				if got := <-keys; got != keyEsc {
					t.Fatalf("standalone escape=%q", got)
				}
			}
			<-calls
			if _, err := io.WriteString(writer, "j"); err != nil {
				t.Fatal(err)
			}
			if got := <-keys; got != "j" {
				t.Fatalf("timeout swallowed subsequent key: %q", got)
			}
			<-calls
			if err := writer.Close(); err != nil {
				t.Fatal(err)
			}
			if _, ok := <-keys; ok {
				t.Fatal("EOF did not close keys")
			}
			stop()
		})
	}
}

// pollThroughSignals blocks like the real poll but retries EINTR itself, so a runtime signal never
// makes readKeys call the fake again and send an acknowledgment the test does not expect.
func pollThroughSignals(fds []unix.PollFd) (int, error) {
	for {
		n, err := unix.Poll(fds, -1)
		if !errors.Is(err, unix.EINTR) {
			return n, err
		}
	}
}

func TestShowJoinsKeyboardOnEveryExit(t *testing.T) {
	t.Parallel()
	for _, exit := range []string{"q", "EOF", "cancel", "clear error", "size error", "frame error"} {
		t.Run(exit, func(t *testing.T) {
			in, writer, err := os.Pipe()
			if err != nil {
				t.Fatal(err)
			}
			defer in.Close()
			defer writer.Close()
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			polling := make(chan struct{}, 1)
			poll := func(fds []unix.PollFd, _ int) (int, error) {
				select {
				case polling <- struct{}{}:
				default:
				}
				return pollThroughSignals(fds)
			}
			keys, stop, err := readKeys(ctx, in, poll)
			if err != nil {
				t.Fatal(err)
			}
			<-polling
			failure := errors.New("terminal failed")
			out := &terminalOutput{err: failure}
			switch exit {
			case "clear error":
				out.failAt = 1
			case "frame error":
				out.failAt = 2
			}
			size := func() (int, int, error) {
				if exit == "size error" {
					return 0, 0, failure
				}
				return 80, 12, nil
			}
			drawn := make(chan struct{})
			frame := func(sc Screen) Drawn {
				close(drawn)
				return Drawn{Lines: []string{"frame"}}
			}
			done := make(chan error, 1)
			go func() {
				done <- show(ctx, out, size, nil, nil, colorprofile.ASCII, frame, options{keys: keys, stopKeys: stop})
			}()
			switch exit {
			case "q", "EOF", "cancel":
				<-drawn
				switch exit {
				case "q":
					_, err = io.WriteString(writer, "q")
				case "EOF":
					err = writer.Close()
				case "cancel":
					cancel()
				}
				if err != nil {
					t.Fatal(err)
				}
			}
			err = <-done
			if strings.Contains(exit, "error") {
				if !errors.Is(err, failure) {
					t.Fatalf("lost terminal failure: %v", err)
				}
			} else if err != nil {
				t.Fatal(err)
			}
			if _, ok := <-keys; ok {
				t.Fatal("show returned before keyboard reader closed")
			}
			if exit != "clear error" && out.writes[len(out.writes)-1] != terminalLeave {
				t.Fatalf("exit failed cursor cleanup: %q", out.writes)
			}
			if exit != "EOF" {
				if _, err := io.WriteString(writer, "remaining input"); err != nil {
					t.Fatalf("reader closed caller-owned input: %v", err)
				}
				buf := make([]byte, len("remaining input"))
				if _, err := io.ReadFull(in, buf); err != nil || string(buf) != "remaining input" {
					t.Fatalf("joined reader consumed subsequent input: %q, %v", buf, err)
				}
			}
		})
	}
}

func TestShowFragmentedArrowStream(t *testing.T) {
	t.Parallel()
	for _, sequence := range []string{"\x1b[A", "\x1bOA"} {
		t.Run(fmt.Sprintf("%q", sequence), func(t *testing.T) {
			in, writer, err := os.Pipe()
			if err != nil {
				t.Fatal(err)
			}
			defer in.Close()
			defer writer.Close()
			calls := make(chan int)
			stopping := make(chan struct{})
			poll := func(fds []unix.PollFd, timeout int) (int, error) {
				select {
				case calls <- timeout:
				case <-stopping:
				}
				return pollThroughSignals(fds)
			}
			keys, stop, err := readKeys(context.Background(), in, poll)
			if err != nil {
				t.Fatal(err)
			}
			out := &terminalOutput{}
			frames := make(chan Screen, 1)
			done := make(chan error, 1)
			go func() {
				done <- show(context.Background(), out, func() (int, int, error) { return 80, 12, nil }, nil, nil, colorprofile.ASCII, func(sc Screen) Drawn {
					frames <- sc
					return Drawn{Lines: []string{"frame"}, Scroll: 20}
				}, options{keys: keys, stopKeys: stop})
			}()
			if sc := <-frames; sc.View != MainView {
				t.Fatalf("initial screen=%v", sc)
			}
			<-calls
			if _, err := io.WriteString(writer, "L"); err != nil {
				t.Fatal(err)
			}
			if sc := <-frames; sc.View != LogView || sc.Scroll != -1 {
				t.Fatalf("log did not open following: %v", sc)
			}
			<-calls
			for i := range len(sequence) {
				if _, err := io.WriteString(writer, sequence[i:i+1]); err != nil {
					t.Fatal(err)
				}
				if i+1 < len(sequence) {
					<-calls
					select {
					case sc := <-frames:
						t.Fatalf("incomplete arrow changed frame: %v", sc)
					default:
					}
				}
			}
			if sc := <-frames; sc.View != LogView || sc.Scroll != 19 {
				t.Fatalf("byte-fragmented arrow returned from log instead of scrolling: %v", sc)
			}
			<-calls
			close(stopping)
			if _, err := io.WriteString(writer, "q"); err != nil {
				t.Fatal(err)
			}
			if err := <-done; err != nil {
				t.Fatal(err)
			}
			if out.writes[len(out.writes)-1] != terminalLeave {
				t.Fatal("stream quit did not clean up terminal")
			}
		})
	}
}

func TestReadKeysCancelsBlockedDelivery(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		in, writer, err := os.Pipe()
		if err != nil {
			t.Fatal(err)
		}
		defer in.Close()
		defer writer.Close()
		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()
		ready := make(chan struct{})
		poll := func(fds []unix.PollFd, timeout int) (int, error) {
			<-ready
			fds[0].Revents = unix.POLLIN
			return 1, nil
		}
		keys, stop, err := readKeys(ctx, in, poll)
		if err != nil {
			t.Fatal(err)
		}
		synctest.Wait()
		if _, err := io.WriteString(writer, "q"); err != nil {
			t.Fatal(err)
		}
		close(ready)
		synctest.Wait()
		// There is no key consumer: the actual pipe read completed and delivery is blocked.
		cancel()
		stop()
		if _, ok := <-keys; ok {
			t.Fatal("cancellation did not join blocked key delivery")
		}
	})
}
