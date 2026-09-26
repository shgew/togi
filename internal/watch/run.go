package watch

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"syscall"
	"time"

	"github.com/charmbracelet/colorprofile"
	"golang.org/x/term"
)

// source reloads the journal only when events.jsonl changed size or modification time since the last read.
type source struct {
	dir  string
	size int64
	mod  time.Time
	ok   bool
	snap Snapshot
}

func (s *source) snapshot() Snapshot {
	info, err := os.Stat(filepath.Join(s.dir, "events.jsonl"))
	if err != nil {
		s.ok = false
		return Load(s.dir)
	}
	if s.ok && info.Size() == s.size && info.ModTime().Equal(s.mod) {
		return s.snap
	}
	s.size, s.mod, s.ok, s.snap = info.Size(), info.ModTime(), true, Load(s.dir)
	return s.snap
}

// Run redraws the dashboard for the journal in dir on out once a second until ctx ends.
func Run(ctx context.Context, dir string, out *os.File) error {
	if _, err := fmt.Fprint(out, "\x1b[?25l\x1b[2J"); err != nil {
		return fmt.Errorf("clear terminal: %w", err)
	}
	defer fmt.Fprint(out, "\x1b[0m\x1b[2J\x1b[H\x1b[?25h")

	winch := make(chan os.Signal, 1)
	signal.Notify(winch, syscall.SIGWINCH)
	defer signal.Stop(winch)
	tick := time.NewTicker(time.Second)
	defer tick.Stop()

	src := source{dir: dir}
	var buf bytes.Buffer
	styled := &colorprofile.Writer{Forward: &buf, Profile: colorprofile.Detect(out, os.Environ())}
	var lastW, lastH int
	for {
		w, h, err := term.GetSize(int(out.Fd()))
		if err != nil {
			return fmt.Errorf("read terminal size: %w", err)
		}
		buf.Reset()
		if w != lastW || h != lastH {
			buf.WriteString("\x1b[2J")
			lastW, lastH = w, h
		}
		buf.WriteString("\x1b[H")
		for i, line := range strings.Split(Render(src.snapshot(), w, h, time.Now()), "\n") {
			if i > 0 {
				buf.WriteString("\n")
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
		case <-tick.C:
		case <-winch:
		}
	}
}
