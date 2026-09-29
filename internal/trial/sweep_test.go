package trial

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"syscall"
	"testing"
	"testing/synctest"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

type staleHost struct {
	fakeHost
	units                        []string
	processes                    []scopeProcess
	alive                        map[int]bool
	killed, stopped              []string
	keepUnit, keepProcess, moved bool
	list                         func(context.Context) ([]string, error)
	kill                         func(context.Context, string) ([]byte, error)
	stop                         func(context.Context, string) ([]byte, error)
}

func (h *staleHost) ListScopes(ctx context.Context) ([]string, error) {
	if h.list != nil {
		return h.list(ctx)
	}
	return slices.Clone(h.units), ctx.Err()
}
func (h *staleHost) ScopeProcesses(ctx context.Context) ([]scopeProcess, error) {
	var out []scopeProcess
	for _, p := range h.processes {
		if h.alive[p.PID] && !h.moved {
			out = append(out, p)
		}
	}
	return out, ctx.Err()
}
func (h *staleHost) ProcessAlive(p scopeProcess) (bool, error) { return h.alive[p.PID], nil }
func (h *staleHost) SignalGroup(pid int, sig syscall.Signal) error {
	h.signals = append(h.signals, fakeSignal{pid, sig})
	if sig == syscall.SIGKILL && !h.keepProcess {
		for _, p := range h.processes {
			if p.Group == pid {
				h.alive[p.PID] = false
			}
		}
	}
	return nil
}
func (h *staleHost) KillScope(ctx context.Context, scope string) ([]byte, error) {
	h.killed = append(h.killed, scope)
	if h.kill != nil {
		return h.kill(ctx, scope)
	}
	return nil, ctx.Err()
}
func (h *staleHost) StopScope(ctx context.Context, scope string) ([]byte, error) {
	h.stopped = append(h.stopped, scope)
	if h.stop != nil {
		return h.stop(ctx, scope)
	}
	if !h.keepUnit {
		h.units = slices.DeleteFunc(h.units, func(s string) bool { return s == scope+".scope" })
	}
	h.moved = h.keepProcess
	return nil, ctx.Err()
}

func TestSweepExactScopes(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		h := &staleHost{units: []string{"togi-trial-0001.scope", "togi-trial-0001-c00.scope", "togi-trial-0001.scope", "togi-preflight-1.scope", "other-togi-trial-1.scope", "togi-trial-*.scope", "togi-trial-?.scope", "togi-trial-[1].scope", "togi-trial-1.scope.extra", "togi-trial-.scope", "togi-trial-/1.scope"}, processes: []scopeProcess{{Scope: "togi-trial-0001-c00.scope", PID: 42, Group: 40}, {Scope: "togi-trial-orphan.scope", PID: 43, Group: 40}}, alive: map[int]bool{42: true, 43: true}}
		r := New(Options{})
		r.host = h
		start := time.Now()
		detail, err := r.Sweep(context.Background())
		if err != nil {
			t.Fatal(err)
		}
		want := []string{"togi-trial-0001", "togi-trial-0001-c00", "togi-trial-orphan"}
		if diff := cmp.Diff(want, h.killed); diff != "" {
			t.Fatalf("killed scopes (-want +got):\n%s", diff)
		}
		if diff := cmp.Diff(want, h.stopped); diff != "" {
			t.Fatalf("stopped scopes (-want +got):\n%s", diff)
		}
		if diff := cmp.Diff([]fakeSignal{{40, syscall.SIGCONT}, {40, syscall.SIGTERM}, {40, syscall.SIGKILL}}, h.signals, cmp.AllowUnexported(fakeSignal{})); diff != "" {
			t.Fatalf("groups (-want +got):\n%s", diff)
		}
		if time.Since(start) != 3*time.Second {
			t.Fatalf("grace = %s", time.Since(start))
		}
		t.Log(detail)
	})
}

func TestSweepCannotConfirmCleanup(t *testing.T) {
	for _, tt := range []struct {
		name          string
		unit, process bool
	}{{"unit remains", true, false}, {"process survives outside scope", false, true}} {
		t.Run(tt.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				h := &staleHost{units: []string{"togi-trial-1.scope"}, processes: []scopeProcess{{Scope: "togi-trial-1.scope", PID: 42, Group: 40}}, alive: map[int]bool{42: true}, keepUnit: tt.unit, keepProcess: tt.process}
				r := New(Options{})
				r.host = h
				start := time.Now()
				_, err := r.Sweep(context.Background())
				if !errors.Is(err, machine.ErrContainment) {
					t.Fatalf("unconfirmed cleanup = %v", err)
				}
				if elapsed := time.Since(start); elapsed > teardownLimit || elapsed < 10*time.Second {
					t.Fatalf("deadline = %s", elapsed)
				}
				t.Logf("%s: containment dead end after %s", tt.name, time.Since(start))
			})
		})
	}
}

func TestSweepSharedDeadline(t *testing.T) {
	for _, phase := range []string{"list", "kill", "stop", "verify"} {
		t.Run(phase, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				h := &staleHost{units: []string{"togi-trial-1.scope", "togi-trial-2.scope", "togi-trial-3.scope"}}
				calls := 0
				blocked := func(ctx context.Context, _ string) ([]byte, error) {
					calls++
					<-ctx.Done()
					return []byte("Unit not loaded"), ctx.Err()
				}
				switch phase {
				case "list", "verify":
					h.list = func(ctx context.Context) ([]string, error) {
						calls++
						if phase == "verify" && calls == 1 {
							time.Sleep(4 * time.Second)
							return h.units, nil
						}
						<-ctx.Done()
						return nil, ctx.Err()
					}
				case "kill":
					h.kill = blocked
				case "stop":
					h.stop = blocked
				}
				r := New(Options{})
				r.host = h
				start := time.Now()
				_, err := r.Sweep(context.Background())
				if !errors.Is(err, machine.ErrContainment) || !errors.Is(err, context.DeadlineExceeded) {
					t.Fatalf("timeout = %v", err)
				}
				want := teardownLimit
				if phase == "kill" || phase == "stop" {
					want = 12 * time.Second
				}
				if phase == "verify" {
					want = 14 * time.Second
				}
				if elapsed := time.Since(start); elapsed != want {
					t.Fatalf("shared bound = %s, want %s", elapsed, want)
				}
				wantCalls := 3
				if phase == "stop" {
					wantCalls = 1
				}
				if (phase == "kill" || phase == "stop") && calls != wantCalls {
					t.Fatalf("attempted calls = %d, want %d", calls, wantCalls)
				}
				t.Logf("%s timeout: containment, %d calls, shared %s", phase, calls, time.Since(start))
			})
		})
	}
}

func TestSweepVerificationError(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		h := &staleHost{}
		calls := 0
		h.list = func(context.Context) ([]string, error) {
			calls++
			if calls == 2 {
				return nil, fmt.Errorf("systemd unavailable")
			}
			return nil, nil
		}
		r := New(Options{})
		r.host = h
		_, err := r.Sweep(context.Background())
		if !errors.Is(err, machine.ErrContainment) {
			t.Fatalf("verification error = %v", err)
		}
	})
}
