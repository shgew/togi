package main

import (
	"context"
	"github.com/shgew/togi/internal/journal"
	"io"
	"os"
	"slices"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/render"
	"github.com/shgew/togi/internal/sim"
	"golang.org/x/sys/unix"
)

// openPTY returns the slave side of a new pseudo-terminal and the master that feeds it typed input.
func openPTY(t *testing.T) (slave, master *os.File) {
	t.Helper()
	master, err := os.OpenFile("/dev/ptmx", os.O_RDWR|syscall.O_NOCTTY, 0)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { master.Close() })
	if err := unix.IoctlSetPointerInt(int(master.Fd()), unix.TIOCSPTLCK, 0); err != nil {
		t.Fatal(err)
	}
	n, err := unix.IoctlGetInt(int(master.Fd()), unix.TIOCGPTN)
	if err != nil {
		t.Fatal(err)
	}
	slave, err = os.OpenFile("/dev/pts/"+strconv.Itoa(n), os.O_RDWR|syscall.O_NOCTTY, 0)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { slave.Close() })
	return slave, master
}

func localFlags(t *testing.T, tty *os.File) uint32 {
	t.Helper()
	termios, err := unix.IoctlGetTermios(int(tty.Fd()), unix.TCGETS)
	if err != nil {
		t.Fatal(err)
	}
	return termios.Lflag
}

func TestDashboardQuietsInputUntilItStops(t *testing.T) {
	for _, tc := range []struct {
		name string
		run  func(context.Context, string, *os.File) error
	}{
		{name: "hidden", run: func(ctx context.Context, _ string, _ *os.File) error { <-ctx.Done(); return nil }},
		{name: "draw error", run: func(context.Context, string, *os.File) error { return os.ErrClosed }},
		{name: "panic", run: func(context.Context, string, *os.File) error { panic("projection bug") }},
	} {
		t.Run(tc.name, func(t *testing.T) {
			slave, _ := openPTY(t)
			before := localFlags(t, slave)
			if before&unix.ECHO == 0 || before&unix.ICANON == 0 {
				t.Fatalf("a new terminal starts echoing in line mode, flags %#x", before)
			}
			out, err := os.CreateTemp(t.TempDir(), "out")
			if err != nil {
				t.Fatal(err)
			}
			defer out.Close()
			d := &dashboard{out: out, in: slave, run: tc.run}
			d.show()
			if tc.name == "hidden" {
				during := localFlags(t, slave)
				if during&(unix.ECHO|unix.ICANON) != 0 || during&unix.ISIG == 0 {
					t.Errorf("while showing, flags %#x: want echo and line mode off, signals on", during)
				}
			}
			d.hide()
			if after := localFlags(t, slave); after != before {
				t.Errorf("after the dashboard stopped, flags %#x, want %#x", after, before)
			}
		})
	}
}

func TestDiscardInputDropsTypedAhead(t *testing.T) {
	slave, master := openPTY(t)
	defer quietInput(slave)()
	if _, err := master.WriteString("y\n"); err != nil {
		t.Fatal(err)
	}
	fds := []unix.PollFd{{Fd: int32(slave.Fd()), Events: unix.POLLIN}}
	if n, err := unix.Poll(fds, 5000); err != nil || n != 1 {
		t.Fatalf("typed input never arrived: %d, %v", n, err)
	}
	if pending, err := unix.IoctlGetInt(int(slave.Fd()), unix.TIOCINQ); err != nil || pending != 2 {
		t.Fatalf("pending input %d, %v; want 2", pending, err)
	}
	if err := discardInput(slave); err != nil {
		t.Fatal(err)
	}
	if pending, err := unix.IoctlGetInt(int(slave.Fd()), unix.TIOCINQ); err != nil || pending != 0 {
		t.Fatalf("pending input after discard %d, %v; want 0", pending, err)
	}
}

type panickingTrials struct{ machine.Trials }

func (panickingTrials) Start(context.Context, machine.TrialSpec) (machine.Running, error) {
	panic("trial start panic")
}

func TestRunHardwareRestoresTerminalWhenSessionPanics(t *testing.T) {
	g := testGlobals(t)
	slave, _ := openPTY(t)
	before := localFlags(t, slave)
	out, err := os.CreateTemp(t.TempDir(), "out")
	if err != nil {
		t.Fatal(err)
	}
	defer out.Close()
	dash := &dashboard{out: out, in: slave, run: func(ctx context.Context, _ string, _ *os.File) error { <-ctx.Done(); return nil }}
	sm, err := sim.New(sim.Config{Seed: 82})
	if err != nil {
		t.Fatal(err)
	}
	newMachine := func(config.Config, string) (machine.Machine, error) {
		seams := sm.Seams()
		seams.Trials = panickingTrials{Trials: seams.Trials}
		return seams, nil
	}
	var recovered any
	func() {
		defer func() { recovered = recover() }()
		runHardware(context.Background(), &g, config.Default(), false, nil, 0, io.Discard, render.Renderer{}, dash, newMachine)
	}()
	if diff := cmp.Diff(any("trial start panic"), recovered); diff != "" {
		t.Errorf("recovered panic (-want +got): %s", diff)
	}
	if diff := cmp.Diff(before, localFlags(t, slave)); diff != "" {
		t.Errorf("terminal flags after the panic (-want +got): %s", diff)
	}
}

// capture collects what the dashboard draws on a pseudo-terminal, so a test can wait for the screen to say something
// instead of for time to pass.
type capture struct {
	mu   sync.Mutex
	cond *sync.Cond
	text string
}

func captureTerminal(t *testing.T, master *os.File) *capture {
	t.Helper()
	c := &capture{}
	c.cond = sync.NewCond(&c.mu)
	go func() {
		buf := make([]byte, 4096)
		for {
			n, err := master.Read(buf)
			c.mu.Lock()
			c.text += string(buf[:n])
			if err != nil {
				c.text += "\x00closed"
			}
			c.cond.Broadcast()
			c.mu.Unlock()
			if err != nil {
				return
			}
		}
	}()
	return c
}

// waitFor returns the screen once any of the words is on it, or once the terminal is closed.
func (c *capture) waitFor(words ...string) string {
	c.mu.Lock()
	defer c.mu.Unlock()
	for !slices.ContainsFunc(words, func(w string) bool { return strings.Contains(c.text, w) }) && !strings.Contains(c.text, "\x00closed") {
		c.cond.Wait()
	}
	return c.text
}

// gatedHost holds the session at its first look at the machine, before it records anything.
type gatedHost struct {
	machine.Host
	wait func()
}

func (h gatedHost) Topology() ([]machine.CoreInfo, error) {
	h.wait()
	return h.Host.Topology()
}

// panicOverStoppedRun stops a session cleanly, then starts another whose trial panics, with the dashboard on a
// pseudo-terminal. It returns the screen when the second run has recorded nothing yet, and the whole screen once the
// panic has propagated out of runHardware.
func panicOverStoppedRun(t *testing.T) (beforeFirstEvent, whole string, recovered any) {
	t.Helper()
	t.Setenv("TERM", "linux")
	t.Setenv("NO_COLOR", "1")
	g := testGlobals(t)
	first, err := sim.New(sim.Config{Seed: 82})
	if err != nil {
		t.Fatal(err)
	}
	stopped, cancel := context.WithCancel(context.Background())
	cancel()
	code := runHardware(stopped, &g, config.Default(), false, nil, 0, io.Discard, render.Renderer{}, nil, func(config.Config, string) (machine.Machine, error) { return first.Seams(), nil })
	if code != exitOK {
		t.Fatalf("the first run exited %d", code)
	}
	if events, _, err := journal.Read(g.stateDir); err != nil || len(events) == 0 || events[len(events)-1].Kind != journal.KindShutdown {
		t.Fatalf("the first run did not leave a stopped session: %d events, %v", len(events), err)
	}

	slave, master := openPTY(t)
	if err := unix.IoctlSetWinsize(int(master.Fd()), unix.TIOCSWINSZ, &unix.Winsize{Row: 45, Col: 160}); err != nil {
		t.Fatal(err)
	}
	screen := captureTerminal(t, master)
	second, err := sim.New(sim.Config{Seed: 82})
	if err != nil {
		t.Fatal(err)
	}
	newMachine := func(config.Config, string) (machine.Machine, error) {
		seams := second.Seams()
		seams.Trials = panickingTrials{Trials: seams.Trials}
		seams.Host = gatedHost{Host: seams.Host, wait: func() { beforeFirstEvent = screen.waitFor("STARTING", "STOPPED", "NO SESSION YET") }}
		return seams, nil
	}
	dash := &dashboard{dir: g.stateDir, out: slave}
	func() {
		defer func() { recovered = recover() }()
		runHardware(context.Background(), &g, config.Default(), false, nil, 0, io.Discard, render.Renderer{}, dash, newMachine)
	}()
	if _, err := slave.WriteString("end of run"); err != nil {
		t.Fatal(err)
	}
	return beforeFirstEvent, screen.waitFor("end of run"), recovered
}

func TestRunHardwareFirstFrameIsThisRunsNotThePreviousOnes(t *testing.T) {
	beforeFirstEvent, _, _ := panicOverStoppedRun(t)
	if !strings.Contains(beforeFirstEvent, "STARTING") || strings.Contains(beforeFirstEvent, "STOPPED") {
		t.Errorf("before this run recorded anything the screen was:\n%q", beforeFirstEvent)
	}
}

func TestRunHardwareRestoresCursorAndPaletteWhenSessionPanics(t *testing.T) {
	_, whole, recovered := panicOverStoppedRun(t)
	if diff := cmp.Diff(any("trial start panic"), recovered); diff != "" {
		t.Errorf("recovered panic (-want +got): %s", diff)
	}
	hide, show := strings.Index(whole, "\x1b[?25l"), strings.LastIndex(whole, "\x1b[?25h")
	if hide < 0 || show < hide {
		t.Errorf("cursor hidden at %d, last shown at %d: %q", hide, show, whole)
	}
	if set, reset := strings.Index(whole, "\x1b]P"), strings.LastIndex(whole, paletteResetEscape); set < 0 || reset < set {
		t.Errorf("palette set at %d, reset at %d: %q", set, reset, whole)
	}
}

const paletteResetEscape = "\x1b]R"
