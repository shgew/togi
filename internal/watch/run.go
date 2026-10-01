package watch

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"syscall"
	"time"

	"github.com/charmbracelet/colorprofile"
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
		s.info = nil
		return Load(s.dir)
	}
	if s.info != nil && os.SameFile(s.info, info) && info.Size() == s.info.Size() && info.ModTime().Equal(s.info.ModTime()) {
		return s.snap
	}
	s.info, s.snap = info, Load(s.dir)
	return s.snap
}

// profile is the colour profile of out; NO_COLOR with any value turns colour off, as no-color.org defines it.
func profile(out *os.File) colorprofile.Profile {
	p := colorprofile.Detect(out, os.Environ())
	if os.Getenv("NO_COLOR") != "" {
		p = min(p, colorprofile.ASCII)
	}
	return p
}

// Run redraws the dashboard for the journal in dir on out once a second until ctx ends.
func Run(ctx context.Context, dir string, out *os.File) error {
	winch := make(chan os.Signal, 1)
	signal.Notify(winch, syscall.SIGWINCH)
	defer signal.Stop(winch)
	tick := time.NewTicker(time.Second)
	defer tick.Stop()
	return run(ctx, dir, out, func() (int, int, error) {
		return term.GetSize(int(out.Fd()))
	}, tick.C, winch, profile(out))
}

func run(ctx context.Context, dir string, out io.Writer, size func() (int, int, error), tick <-chan time.Time, winch <-chan os.Signal, p colorprofile.Profile) error {
	if _, err := fmt.Fprint(out, "\x1b[?25l\x1b[2J"); err != nil {
		return fmt.Errorf("clear terminal: %w", err)
	}
	defer fmt.Fprint(out, "\x1b[0m\x1b[2J\x1b[H\x1b[?25h")

	src := source{dir: dir}
	var buf bytes.Buffer
	styled := &colorprofile.Writer{Forward: &buf, Profile: p}
	var lastW, lastH int
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
		case <-tick:
		case <-winch:
		}
	}
}
