package trial

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sync"
	"syscall"
	"time"
)

type fakeSignal struct {
	pid int
	sig syscall.Signal
}

type fakeHost struct {
	mu        sync.Mutex
	procs     []*fakeProc
	signals   []fakeSignal
	inScope   bool
	killScope func(string) ([]byte, error)
	startFail int
}

type fakeProc struct {
	host             *fakeHost
	pid              int
	stdoutR, stderrR *io.PipeReader
	stdoutW, stderrW *io.PipeWriter
	exited           chan struct{}
	once             sync.Once
	exitErr          error
	busy, stopped    bool
	resume           chan struct{}
	cpu              time.Duration
	since            time.Time
	escapeCPU        int
	holdOutput       bool
}

func (h *fakeHost) Start(_ context.Context, argv []string, dir string) (process, error) {
	if h.startFail > 0 && len(h.procs)+1 == h.startFail {
		return nil, errors.New("injected start failure")
	}
	stdoutR, stdoutW := io.Pipe()
	stderrR, stderrW := io.Pipe()
	h.mu.Lock()
	p := &fakeProc{host: h, pid: 1000 + len(h.procs), stdoutR: stdoutR, stdoutW: stdoutW, stderrR: stderrR, stderrW: stderrW, exited: make(chan struct{})}
	p.holdOutput = argv[len(argv)-1] == "held-output"
	h.procs = append(h.procs, p)
	h.mu.Unlock()
	go p.script(argv[len(argv)-1], dir)
	return p, nil
}

func (p *fakeProc) PID() int              { return p.pid }
func (p *fakeProc) Stdout() io.ReadCloser { return p.stdoutR }
func (p *fakeProc) Stderr() io.ReadCloser { return p.stderrR }
func (p *fakeProc) Wait() error {
	<-p.exited
	p.host.mu.Lock()
	defer p.host.mu.Unlock()
	return p.exitErr
}

func (p *fakeProc) finish(err error) {
	p.once.Do(func() {
		p.host.mu.Lock()
		p.accrue(time.Now())
		p.busy = false
		p.exitErr = err
		close(p.exited)
		p.host.mu.Unlock()
		if !p.holdOutput {
			p.stdoutW.Close()
			p.stderrW.Close()
		}
	})
}

func (p *fakeProc) accrue(now time.Time) {
	if p.busy && !p.stopped {
		p.cpu += now.Sub(p.since)
	}
	p.since = now
}

func (p *fakeProc) setBusy(escape int) {
	p.host.mu.Lock()
	p.busy = true
	p.since = time.Now()
	p.escapeCPU = escape
	p.host.mu.Unlock()
}

func (p *fakeProc) sleep(d time.Duration) bool {
	select {
	case <-time.After(d):
	case <-p.exited:
		return false
	}
	p.host.mu.Lock()
	resume := p.resume
	p.host.mu.Unlock()
	if resume == nil {
		return true
	}
	select {
	case <-resume:
		return true
	case <-p.exited:
		return false
	}
}

func (p *fakeProc) script(mode, dir string) {
	defer p.finish(nil)
	switch mode {
	case "work", "escape":
		if mode == "escape" {
			p.setBusy(9)
		} else {
			p.setBusy(0)
		}
		for p.sleep(100 * time.Millisecond) {
			if _, err := fmt.Fprintln(p.stdoutW, "progress 1"); err != nil {
				return
			}
		}
	case "error":
		if p.sleep(300 * time.Millisecond) {
			fmt.Fprintln(p.stdoutW, "COMPUTE ERROR")
		}
	case "watched", "watched-precedence":
		text := "COMPUTE ERROR\n"
		if mode == "watched-precedence" {
			text = "PIN FAILED\nCOMPUTE ERROR\nAFFINITY:42\n"
		}
		if err := os.WriteFile(filepath.Join(dir, "results.txt"), []byte(text), 0644); err != nil {
			fmt.Fprintln(p.stderrW, err)
		}
	case "setup-then-error":
		fmt.Fprint(p.stdoutW, "PIN FAILED\nCOMPUTE ERROR\n")
	case "error-then-affinity":
		fmt.Fprint(p.stdoutW, "COMPUTE ERROR\nAFFINITY:42\n")
	case "sleep":
		<-p.exited
	case "read-error":
		p.stdoutW.CloseWithError(syscall.EIO)
		<-p.exited
	case "exit":
	case "held-output":
		fmt.Fprint(p.stdoutW, "COMPUTE ERROR")
	}
}

func (h *fakeHost) SignalGroup(pid int, sig syscall.Signal) error {
	h.mu.Lock()
	var p *fakeProc
	for _, candidate := range h.procs {
		if candidate.pid == pid {
			p = candidate
			break
		}
	}
	if p == nil {
		h.mu.Unlock()
		return fmt.Errorf("signal pid %d: %w", pid, os.ErrNotExist)
	}
	h.signals = append(h.signals, fakeSignal{pid, sig})
	select {
	case <-p.exited:
		h.mu.Unlock()
		return nil
	default:
	}
	p.accrue(time.Now())
	switch {
	case sig == syscall.SIGSTOP && !p.stopped:
		p.stopped = true
		p.resume = make(chan struct{})
	case sig == syscall.SIGCONT && p.stopped:
		p.stopped = false
		close(p.resume)
		p.resume = nil
	}
	h.mu.Unlock()
	if sig == syscall.SIGTERM || sig == syscall.SIGKILL {
		p.finish(errors.New("signal: killed"))
	}
	return nil
}

func (h *fakeHost) InScope(int, string) bool {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.inScope
}
func (h *fakeHost) Usage(pid int) (usage, error) {
	h.mu.Lock()
	defer h.mu.Unlock()
	for _, p := range h.procs {
		if p.pid == pid {
			p.accrue(time.Now())
			return usage{CPUTime: p.cpu, CPU: p.escapeCPU}, nil
		}
	}
	return usage{}, fmt.Errorf("read pid %d usage: %w", pid, os.ErrNotExist)
}
func (h *fakeHost) Threads(pid int) ([]thread, error) {
	h.mu.Lock()
	defer h.mu.Unlock()
	for _, p := range h.procs {
		if p.pid == pid && p.escapeCPU != 0 {
			return []thread{{TID: pid, CPU: p.escapeCPU}}, nil
		}
	}
	return nil, nil
}
func (h *fakeHost) KillScope(ctx context.Context, scope string) ([]byte, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	h.mu.Lock()
	kill := h.killScope
	h.mu.Unlock()
	if kill != nil {
		return kill(scope)
	}
	return nil, nil
}

func (h *fakeHost) recordedSignals() []fakeSignal {
	h.mu.Lock()
	defer h.mu.Unlock()
	return append([]fakeSignal(nil), h.signals...)
}

func (*fakeHost) ListScopes(ctx context.Context) ([]string, error) {
	return nil, ctx.Err()
}
func (*fakeHost) ScopeProcesses(ctx context.Context) ([]scopeProcess, error) {
	return nil, ctx.Err()
}
func (*fakeHost) ProcessAlive(scopeProcess) (bool, error) { return false, nil }
func (*fakeHost) StopScope(ctx context.Context, _ string) ([]byte, error) {
	return nil, ctx.Err()
}
