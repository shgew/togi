package watch

import (
	"context"
	"errors"
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
		if !strings.Contains(terminalFrame(t, out, 2, 160, 40, false), "invalid character") {
			t.Fatal("live journal read error was not displayed")
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
