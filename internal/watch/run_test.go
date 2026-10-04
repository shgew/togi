package watch

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"strings"
	"syscall"
	"testing"
	"testing/synctest"
	"time"

	"github.com/charmbracelet/colorprofile"
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

func TestShowWritesOnlyChangedRows(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()
		tick := make(chan time.Time)
		winch := make(chan os.Signal)
		keys := make(chan key)
		out := &terminalOutput{}
		lines := []string{"first", "second", "third"}
		w, h := 80, 12
		done := make(chan error, 1)
		go func() {
			done <- show(ctx, out, func() (int, int, error) { return w, h, nil }, tick, winch, colorprofile.ASCII,
				func(Screen) Drawn { return Drawn{Lines: lines} }, options{keys: keys, palette: true})
		}()
		check := func(want ...string) {
			t.Helper()
			synctest.Wait()
			if diff := cmp.Diff(want, out.writes); diff != "" {
				t.Fatalf("terminal writes (-want +got):\n%s", diff)
			}
		}
		initial := "\x1b[2J\x1b[1;1Hfirst\x1b[K\x1b[2;1Hsecond\x1b[K\x1b[3;1Hthird\x1b[K"
		check(paletteSet()+terminalEnter, initial)
		lines[1] = "short"
		tick <- time.Time{}
		update := "\x1b[2;1Hshort\x1b[K"
		check(paletteSet()+terminalEnter, initial, update)
		tick <- time.Time{}
		check(paletteSet()+terminalEnter, initial, update)
		lines = lines[:1]
		tick <- time.Time{}
		shorter := "\x1b[2;1H\x1b[K\x1b[3;1H\x1b[K"
		check(paletteSet()+terminalEnter, initial, update, shorter)
		w, h = 100, 20
		winch <- syscall.SIGWINCH
		full := "\x1b[2J\x1b[1;1Hfirst\x1b[K"
		check(paletteSet()+terminalEnter, initial, update, shorter, full)
		keys <- "?"
		check(paletteSet()+terminalEnter, initial, update, shorter, full, full)
		cancel()
		if err := <-done; err != nil {
			t.Fatal(err)
		}
		check(paletteSet()+terminalEnter, initial, update, shorter, full, full, terminalLeave+paletteReset)
	})
}

func TestLiveHoldsUntilJournalKeyResizeOrDeadline(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()
		changes := make(chan error)
		keys := make(chan key)
		winch := make(chan os.Signal)
		out := &terminalOutput{}
		var until time.Time
		frames, sizes, reloads := 0, 0, 0
		done := make(chan error, 1)
		go func() {
			done <- show(ctx, out, func() (int, int, error) {
				sizes++
				return 80, 12, nil
			}, nil, winch, colorprofile.ASCII, func(Screen) Drawn {
				frames++
				return Drawn{Lines: []string{fmt.Sprint(frames)}, Until: until}
			}, options{live: true, changes: changes, keys: keys, reload: func() bool { reloads++; return true }})
		}()
		check := func(wantFrames, wantReloads int) {
			t.Helper()
			synctest.Wait()
			if diff := cmp.Diff([]int{wantFrames, wantFrames, wantReloads}, []int{frames, sizes, reloads}); diff != "" {
				t.Fatalf("redraws, size reads, journal reloads (-want +got):\n%s", diff)
			}
		}
		check(1, 0)
		time.Sleep(time.Second)
		check(2, 0)
		until = time.Now().Add(20 * time.Second)
		changes <- nil
		check(3, 1)
		time.Sleep(5 * time.Second)
		check(3, 1)
		changes <- nil
		check(4, 2)
		keys <- "?"
		check(5, 2)
		winch <- syscall.SIGWINCH
		check(6, 2)
		time.Sleep(14 * time.Second)
		check(6, 2)
		time.Sleep(time.Second)
		check(7, 2)
		time.Sleep(time.Second)
		check(8, 2)
		cancel()
		if err := <-done; err != nil {
			t.Fatal(err)
		}
	})
}

func TestRefreshClockStopsTickerDuringHold(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		var clock refreshClock
		defer clock.stop()
		periodic := clock.schedule(time.Time{})
		until := time.Now().Add(10 * time.Second)
		deadline := clock.schedule(until)
		time.Sleep(9 * time.Second)
		for name, ch := range map[string]<-chan time.Time{"periodic": periodic, "deadline": deadline} {
			select {
			case <-ch:
				t.Fatalf("%s woke during the hold", name)
			default:
			}
		}
		time.Sleep(time.Second)
		if got := <-deadline; !got.Equal(until) {
			t.Fatalf("deadline=%s, want %s", got, until)
		}
	})
}

func TestShowIgnoresUntil(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()
		tick := make(chan time.Time)
		frames := 0
		done := make(chan error, 1)
		go func() {
			done <- show(ctx, io.Discard, func() (int, int, error) { return 80, 12, nil }, tick, nil, colorprofile.ASCII,
				func(Screen) Drawn {
					frames++
					return Drawn{Lines: []string{"replay"}, Until: time.Now().Add(time.Hour)}
				}, options{})
		}()
		synctest.Wait()
		for range 2 {
			tick <- time.Time{}
			synctest.Wait()
		}
		if diff := cmp.Diff(3, frames); diff != "" {
			t.Fatalf("caller-tick frames (-want +got):\n%s", diff)
		}
		cancel()
		if err := <-done; err != nil {
			t.Fatal(err)
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
				frame := func(Screen) Drawn { return Drawn{Lines: []string{fmt.Sprint(calls)}} }
				go func() { done <- show(ctx, out, size, nil, winch, colorprofile.ASCII, frame, options{}) }()
				synctest.Wait()
				if tc.redraw {
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
		lastWrites := 0
		check := func(view View, scroll int, clear bool) {
			t.Helper()
			synctest.Wait()
			want := Screen{View: view, Scroll: scroll, Width: 80, Height: 12, Keys: true}
			if diff := cmp.Diff(want, screens[len(screens)-1]); diff != "" {
				t.Fatalf("consumer frame (-want +got):\n%s", diff)
			}
			text := out.writes[len(out.writes)-1]
			if len(out.writes) != lastWrites && strings.HasPrefix(text, "\x1b[2J") != clear {
				t.Fatalf("view change clear=%t: %q", clear, text)
			}
			if !strings.Contains(text, fmt.Sprintf("view %d scroll %d", view, scroll)) {
				t.Fatalf("consumer frame not drawn: %q", text)
			}
			lastWrites = len(out.writes)
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
