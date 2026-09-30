//go:build integration && linux

package trial

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/shgew/togi/internal/machine"
)

func TestProcessTrials(t *testing.T) {
	for _, tt := range []struct {
		name, mode string
		duration   time.Duration
		signal     machine.Signal
	}{
		{"pass", "work", 600 * time.Millisecond, ""},
		{"error", "error", time.Second, machine.ComputationError},
		{"exit", "exit", time.Second, machine.UnexpectedExit},
	} {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			o := testOptions(t, tt.mode)
			spec := testSpec(tt.name, machine.R1, tt.duration)
			spec.CPUs = []int{o.Cores[0].CPUs[0]}
			running, err := New(o).Start(context.Background(), spec)
			if err != nil {
				t.Fatal(err)
			}
			rec := &recorder{}
			result, err := running.Wait(context.Background(), rec)
			if err != nil || result.Signal != tt.signal || len(result.Escaped) != 0 {
				t.Fatalf("result %+v err %v", result, err)
			}
			if tt.name == "pass" && len(rec.progress) == 0 {
				t.Fatal("no progress captured")
			}
		})
	}
}

func TestProcessOversizedOutput(t *testing.T) {
	for _, mode := range []string{"oversized-stdout", "oversized-stderr", "watched-oversized"} {
		t.Run(mode, func(t *testing.T) {
			o := testOptions(t, mode)
			spec := testSpec(mode, machine.R1, time.Minute)
			spec.CPUs = []int{o.Cores[0].CPUs[0]}
			started, err := New(o).Start(context.Background(), spec)
			if err != nil {
				t.Fatal(err)
			}
			trial := started.(*running)
			t.Cleanup(func() { _ = trial.Stop() })
			begin := time.Now()
			result, err := trial.Wait(context.Background(), &recorder{})
			elapsed := time.Since(begin)
			if !errors.Is(err, errOutputLineTooLong) || errors.Is(err, machine.ErrContainment) || result.Inconclusive == "" || result.Signal != "" {
				t.Fatalf("oversized helper result=%+v err=%v", result, err)
			}
			if elapsed > teardownLimit {
				t.Fatalf("oversized helper teardown exceeded deadline: %s", elapsed)
			}
			for _, inst := range trial.instances {
				select {
				case <-inst.joined:
				default:
					t.Fatal("helper process or output readers remain")
				}
				if alive, err := trial.host.ProcessAlive(scopeProcess{PID: inst.PID, Start: inst.process.(*execProcess).start}); err != nil || alive {
					t.Fatalf("helper remains: alive=%t err=%v", alive, err)
				}
			}
			var prefixSize int
			if mode == "watched-oversized" {
				prefixSize = len(trial.instances[0].watch[0].lines.pending)
			} else {
				name := strings.TrimPrefix(mode, "oversized-") + ".log"
				info, err := os.Stat(filepath.Join(o.Dir, spec.ID, name))
				if err != nil {
					t.Fatal(err)
				}
				prefixSize = int(info.Size())
			}
			if prefixSize != outputLineLimit {
				t.Fatalf("diagnostic prefix = %d bytes, want %d", prefixSize, outputLineLimit)
			}
			t.Logf("%s: inconclusive (%v); retained %d-byte prefix; process exited and output readers joined in %.3fs; no sudo or host scopes", mode, err, prefixSize, elapsed.Seconds())
		})
	}
}

func TestProcessEscape(t *testing.T) {
	o := testOptions(t, "")
	o.Backends[machine.Mprime] = helperBackend{mode: fmt.Sprintf("escape:%d", o.Cores[1].CPUs[0])}
	spec := testSpec("escape", machine.R1, time.Second)
	spec.CPUs = []int{o.Cores[0].CPUs[0]}
	running, err := New(o).Start(context.Background(), spec)
	if err != nil {
		t.Fatal(err)
	}
	rec := &recorder{}
	result, err := running.Wait(context.Background(), rec)
	if err != nil {
		t.Fatal(err)
	}
	if result.Inconclusive != "" && strings.Contains(result.Inconclusive, "PIN FAILED") {
		t.Skip("pinning unavailable")
	}
	if !slices.Equal(result.Escaped, []int{o.Cores[1].CPUs[0]}) || len(rec.samples) == 0 || rec.samples[0].Warning != "outside allowed cpus" {
		t.Fatalf("escape result %+v samples %+v", result, rec.samples)
	}
}

func TestProcessR6StartsStopped(t *testing.T) {
	o := testOptions(t, "work")
	o.SampleInterval = 450 * time.Millisecond
	spec := testSpec("r6-idle", machine.R6, 600*time.Millisecond)
	spec.Cores = []int{0, 1}
	spec.CPUs = []int{o.Cores[0].CPUs[0], o.Cores[1].CPUs[0]}
	r, err := New(o).Start(context.Background(), spec)
	if err != nil {
		t.Fatal(err)
	}
	trial := r.(*running)
	for _, inst := range trial.instances {
		deadline := time.Now().Add(time.Second)
		for {
			fields, err := procStat(fmt.Sprintf("/proc/%d/stat", inst.PID))
			if err == nil && fields[0] == "T" {
				break
			}
			if time.Now().After(deadline) {
				_ = trial.abort(nil)
				t.Fatalf("core %02d not initially stopped: state %v, err %v", inst.Core, fields, err)
			}
			runtime.Gosched()
		}
	}
	var rec recorder
	result, err := trial.Wait(context.Background(), &rec)
	if err != nil || result.Signal != "" || result.Stops != 3 || result.Conts != 1 {
		t.Fatalf("R6 initial stops and bursts result %+v, err %v", result, err)
	}
}

type rollbackProcessHost struct {
	osHost
	starts    int
	scopes    []string
	orphan    int
	failureAt time.Time
}

func (h *rollbackProcessHost) Start(ctx context.Context, argv []string, dir string) (process, error) {
	h.starts++
	if h.starts == 2 {
		h.failureAt = time.Now()
		return nil, errors.New("injected second-instance start failure")
	}
	p, err := h.osHost.Start(ctx, argv[slices.Index(argv, "--")+1:], dir)
	if err != nil {
		return nil, err
	}
	deadline := time.Now().Add(3 * time.Second)
	for {
		data, err := os.ReadFile(filepath.Join(dir, "descendant.pid"))
		if err == nil {
			h.orphan, err = strconv.Atoi(string(data))
			if err != nil {
				return nil, err
			}
			return p, nil
		}
		if time.Now().After(deadline) {
			_ = h.SignalGroup(p, syscall.SIGKILL)
			p.Stdout().Close()
			p.Stderr().Close()
			_ = p.Wait()
			return nil, fmt.Errorf("wait for detached descendant: %w", err)
		}
		time.Sleep(time.Millisecond)
	}
}

func (*rollbackProcessHost) InScope(int, string) bool { return true }

func (h *rollbackProcessHost) KillScope(ctx context.Context, scope string) ([]byte, error) {
	h.scopes = append(h.scopes, scope)
	return nil, ctx.Err()
}

func TestProcessPartialStartRollback(t *testing.T) {
	o := testOptions(t, "pipe-descendant")
	o.NoScope = false
	h := &rollbackProcessHost{}
	t.Cleanup(func() {
		if h.orphan != 0 {
			_ = syscall.Kill(h.orphan, syscall.SIGKILL)
		}
	})
	r := New(o)
	r.host = h
	spec := testSpec("pipe-rollback", machine.R7, time.Second)
	spec.Cores = []int{0, 1}
	spec.CPUs = []int{o.Cores[0].CPUs[0], o.Cores[1].CPUs[0]}
	_, err := r.Start(context.Background(), spec)
	elapsed := time.Since(h.failureAt)
	if !errors.Is(err, machine.ErrContainment) || !strings.Contains(err.Error(), "injected second-instance start failure") {
		t.Fatalf("rollback = %v", err)
	}
	if h.failureAt.IsZero() || elapsed > 15*time.Second+500*time.Millisecond {
		t.Fatalf("rollback exceeded deadline: %s", elapsed)
	}
	if !slices.Equal(h.scopes, []string{"togi-trial-pipe-rollback-c00", "togi-trial-pipe-rollback-c01"}) {
		t.Fatalf("scope cleanup = %v", h.scopes)
	}
	if err := syscall.Kill(h.orphan, 0); err != nil {
		t.Fatalf("detached pipe holder exited before the deadline proof: %v", err)
	}
	t.Logf("second launch failed; %d known scope-kill calls; detached pipe holder forced closed; containment error after %.3fs", len(h.scopes), elapsed.Seconds())
}

type orphanSweepHost struct {
	osHost
	unit         string
	orphan       scopeProcess
	descendant   *os.Process
	kills, stops int
}

func (h *orphanSweepHost) ListScopes(ctx context.Context) ([]string, error) {
	if h.unit == "" {
		return nil, ctx.Err()
	}
	return []string{h.unit}, ctx.Err()
}

func (h *orphanSweepHost) ScopeProcesses(ctx context.Context) ([]scopeProcess, error) {
	alive, err := h.ProcessAlive(h.orphan)
	if err != nil || !alive {
		return nil, err
	}
	return []scopeProcess{h.orphan}, ctx.Err()
}

func (h *orphanSweepHost) KillScope(ctx context.Context, scope string) ([]byte, error) {
	h.kills++
	return h.SignalScope(ctx, scope, syscall.SIGKILL)
}

func (h *orphanSweepHost) StopScope(ctx context.Context, _ string) ([]byte, error) {
	h.stops++
	h.unit = ""
	return nil, ctx.Err()
}

func (h *orphanSweepHost) SignalScope(ctx context.Context, scope string, sig syscall.Signal) ([]byte, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if h.orphan.Scope != scope+".scope" {
		return nil, errors.New("unowned helper scope")
	}
	alive, err := h.ProcessAlive(h.orphan)
	if err != nil || !alive {
		return nil, err
	}
	return nil, h.descendant.Signal(sig)
}

func TestProcessSweepAfterKilledOwner(t *testing.T) {
	o := testOptions(t, "scope-owner")
	launch, err := o.Backends[machine.Mprime].Prepare(machine.Workload{}, o.Dir, o.Cores[0].CPUs)
	if err != nil {
		t.Fatal(err)
	}
	h := &orphanSweepHost{unit: "togi-trial-leftover.scope"}
	owner, err := h.Start(context.Background(), launch.Argv, o.Dir)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_ = h.SignalGroup(owner, syscall.SIGKILL)
		if h.descendant != nil {
			_ = h.descendant.Kill()
		}
		owner.Stdout().Close()
		owner.Stderr().Close()
	})
	deadline := time.Now().Add(3 * time.Second)
	for {
		data, err := os.ReadFile(filepath.Join(o.Dir, "descendant.pid"))
		if err == nil {
			h.orphan.PID, err = strconv.Atoi(string(data))
			if err != nil {
				t.Fatal(err)
			}
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("wait for owner's descendant: %v", err)
		}
		time.Sleep(time.Millisecond)
	}
	fields, err := procStat(fmt.Sprintf("/proc/%d/stat", h.orphan.PID))
	if err != nil {
		t.Fatal(err)
	}
	h.orphan.Scope = h.unit
	h.orphan.Start, err = fieldInt(fields, 22)
	if err != nil {
		t.Fatal(err)
	}
	h.descendant, err = os.FindProcess(h.orphan.PID)
	if err != nil {
		t.Fatal(err)
	}
	if err := h.SignalGroup(owner, syscall.SIGKILL); err != nil {
		t.Fatal(err)
	}
	_ = owner.Wait()
	if alive, err := h.ProcessAlive(h.orphan); err != nil || !alive {
		t.Fatalf("detached descendant did not survive killed owner: alive=%t err=%v", alive, err)
	}
	r := New(o)
	r.host = h
	started := time.Now()
	detail, err := r.Sweep(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if alive, err := h.ProcessAlive(h.orphan); err != nil || alive || h.unit != "" || h.kills != 1 || h.stops != 1 {
		t.Fatalf("leftover cleanup: alive=%t unit=%q kills=%d stops=%d err=%v", alive, h.unit, h.kills, h.stops, err)
	}
	if elapsed := time.Since(started); elapsed > teardownLimit {
		t.Fatalf("sweep exceeded deadline: %s", elapsed)
	}
	t.Logf("killed helper owner; detached real process removed; %s; systemd unit operations faked, no host scopes touched", detail)
}

func TestProcessOwnedGroupIdentity(t *testing.T) {
	o := testOptions(t, "sleep")
	launch, err := o.Backends[machine.Mprime].Prepare(machine.Workload{}, o.Dir, o.Cores[0].CPUs)
	if err != nil {
		t.Fatal(err)
	}
	h := osHost{}
	owned, err := h.Start(context.Background(), launch.Argv, o.Dir)
	if err != nil {
		t.Fatal(err)
	}
	p := owned.(*execProcess)
	waited := false
	t.Cleanup(func() {
		_ = p.cmd.Process.Kill()
		if !waited {
			_ = p.Wait()
		}
		p.Stdout().Close()
		p.Stderr().Close()
	})
	stale := &execProcess{cmd: p.cmd, start: p.start + 1}
	if err := h.SignalGroup(stale, syscall.SIGKILL); !errors.Is(err, syscall.ESRCH) {
		t.Fatalf("stale launch identity signaled a live helper: %v", err)
	}
	if alive, err := h.ProcessAlive(scopeProcess{PID: p.PID(), Start: p.start}); err != nil || !alive {
		t.Fatalf("unrelated identity did not survive: alive=%t err=%v", alive, err)
	}
	if err := h.SignalGroup(p, syscall.SIGTERM); err != nil {
		t.Fatal(err)
	}
	_ = p.Wait()
	waited = true
	if err := h.SignalGroup(p, syscall.SIGKILL); !errors.Is(err, syscall.ESRCH) {
		t.Fatalf("reaped process group was signaled: %v", err)
	}
	t.Log("stale launch identity left real helper alive; verified owner terminated it; reaped PID refused a later group signal")
}
