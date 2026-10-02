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
	scopeSignals                 []scopeSignal
	signal                       func(context.Context, string, syscall.Signal) ([]byte, error)
}

type scopeSignal struct {
	scope string
	sig   syscall.Signal
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
func (h *staleHost) SignalGroup(p process, sig syscall.Signal) error {
	h.signals = append(h.signals, fakeSignal{p.PID(), sig})
	return errors.New("recovered process must not receive process-group signals")
}
func (h *staleHost) SignalScope(ctx context.Context, scope string, sig syscall.Signal) ([]byte, error) {
	h.scopeSignals = append(h.scopeSignals, scopeSignal{scope, sig})
	if h.signal != nil {
		return h.signal(ctx, scope, sig)
	}
	return nil, ctx.Err()
}
func (h *staleHost) KillScope(ctx context.Context, scope string) ([]byte, error) {
	h.killed = append(h.killed, scope)
	if h.kill != nil {
		return h.kill(ctx, scope)
	}
	if !h.keepProcess {
		for _, p := range h.processes {
			if p.Scope == scope+".scope" {
				h.alive[p.PID] = false
			}
		}
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
		h := &staleHost{units: []string{"togi-trial-0001.scope", "togi-trial-0001-c00.scope", "togi-trial-0001.scope", "togi-preflight-1.scope", "other-togi-trial-1.scope", "togi-trial-*.scope", "togi-trial-?.scope", "togi-trial-[1].scope", "togi-trial-1.scope.extra", "togi-trial-.scope", "togi-trial-/1.scope"}, processes: []scopeProcess{{Scope: "togi-trial-0001-c00.scope", PID: 42}, {Scope: "togi-trial-orphan.scope", PID: 43}}, alive: map[int]bool{42: true, 43: true}}
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
		if len(h.signals) != 0 {
			t.Fatalf("recovered PIDs received process-group signals: %v", h.signals)
		}
		var wantSignals []scopeSignal
		for _, sig := range []syscall.Signal{syscall.SIGCONT, syscall.SIGTERM} {
			for _, scope := range want {
				wantSignals = append(wantSignals, scopeSignal{scope, sig})
			}
		}
		if diff := cmp.Diff(wantSignals, h.scopeSignals, cmp.AllowUnexported(scopeSignal{})); diff != "" {
			t.Fatalf("exact scope signals (-want +got):\n%s", diff)
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
				h := &staleHost{units: []string{"togi-trial-1.scope"}, processes: []scopeProcess{{Scope: "togi-trial-1.scope", PID: 42}}, alive: map[int]bool{42: true}, keepUnit: tt.unit, keepProcess: tt.process}
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

func TestSweepScopeSignalDeadline(t *testing.T) {
	for _, sig := range []syscall.Signal{syscall.SIGCONT, syscall.SIGTERM} {
		t.Run(sig.String(), func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				h := &staleHost{units: []string{"togi-trial-1.scope", "togi-trial-2.scope", "togi-trial-3.scope"}}
				h.signal = func(ctx context.Context, _ string, sent syscall.Signal) ([]byte, error) {
					if sent == sig {
						<-ctx.Done()
					}
					return []byte("Unit not loaded"), ctx.Err()
				}
				r := New(Options{})
				r.host = h
				start := time.Now()
				_, err := r.Sweep(context.Background())
				if !errors.Is(err, machine.ErrContainment) || !errors.Is(err, context.DeadlineExceeded) {
					t.Fatalf("signal timeout accepted as missing scope: %v", err)
				}
				if elapsed := time.Since(start); elapsed != 3*time.Second {
					t.Fatalf("shared scope-signaling grace = %s", elapsed)
				}
				if len(h.scopeSignals) != 6 || len(h.killed) != 3 || len(h.stopped) != 3 || len(h.signals) != 0 {
					t.Fatalf("scope cleanup attempts signals=%v kills=%v stops=%v groups=%v", h.scopeSignals, h.killed, h.stopped, h.signals)
				}
				t.Logf("scope %s timeout: containment; six exact scope signals, three kills and stops, shared 3s grace; zero recovered group signals", sig)
			})
		})
	}
}

func TestSweepScopeSignalFailure(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		h := &staleHost{units: []string{"togi-trial-1.scope"}}
		h.signal = func(context.Context, string, syscall.Signal) ([]byte, error) {
			return nil, errors.New("systemd signal failed")
		}
		r := New(Options{})
		r.host = h
		_, err := r.Sweep(context.Background())
		if !errors.Is(err, machine.ErrContainment) || len(h.units) != 0 || len(h.killed) != 1 || len(h.stopped) != 1 {
			t.Fatalf("uncertain signaling must fail despite final cleanup: err=%v units=%v kills=%v stops=%v", err, h.units, h.killed, h.stopped)
		}
	})
}

type observationFailureHost struct {
	*staleHost
	discoveryError error
	aliveError     error
	discoveries    int
	failAfter      int
}

func (h *observationFailureHost) ScopeProcesses(ctx context.Context) ([]scopeProcess, error) {
	h.discoveries++
	if h.discoveryError != nil && h.discoveries >= h.failAfter {
		return nil, h.discoveryError
	}
	return h.staleHost.ScopeProcesses(ctx)
}

func (h *observationFailureHost) ProcessAlive(p scopeProcess) (bool, error) {
	if h.aliveError != nil {
		return false, h.aliveError
	}
	return h.staleHost.ProcessAlive(p)
}

func TestSweepObservationFailureIsContainment(t *testing.T) {
	for _, failure := range []string{"discovery", "verify discovery", "verify identity"} {
		t.Run(failure, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				injected := errors.New("leftover observation lost")
				h := &observationFailureHost{staleHost: &staleHost{processes: []scopeProcess{{Scope: "togi-trial-1.scope", PID: 42}, {Scope: "unrelated.scope", PID: 43}}, alive: map[int]bool{42: true, 43: true}}}
				switch failure {
				case "discovery":
					h.discoveryError, h.failAfter = injected, 1
				case "verify discovery":
					h.discoveryError, h.failAfter = injected, 2
				case "verify identity":
					h.aliveError = injected
				}
				r := New(Options{})
				r.host = h
				_, err := r.Sweep(context.Background())
				if !errors.Is(err, injected) || !errors.Is(err, machine.ErrContainment) {
					t.Fatalf("lost sweep observation accepted: %v", err)
				}
				if failure != "discovery" && len(h.killed) != 1 {
					t.Fatalf("observation loss skipped cleanup: %v", h.killed)
				}
				if !h.alive[43] {
					t.Fatal("unrelated process was killed")
				}
			})
		})
	}
}
