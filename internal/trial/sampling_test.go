package trial

import (
	"context"
	"errors"
	"fmt"
	"os"
	"testing"
	"testing/synctest"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

type samplingHost struct {
	*fakeHost
	threads func(int) ([]thread, error)
	usage   func(int) (usage, error)
}

func (h samplingHost) Threads(pid int) ([]thread, error) {
	if h.threads != nil {
		return h.threads(pid)
	}
	return h.fakeHost.Threads(pid)
}

func (h samplingHost) Usage(pid int) (usage, error) {
	if h.usage != nil {
		return h.usage(pid)
	}
	return h.fakeHost.Usage(pid)
}

func TestSamplingLoss(t *testing.T) {
	for _, read := range []string{"threads", "usage"} {
		for _, transient := range []bool{false, true} {
			t.Run(fmt.Sprintf("%s/transient=%t", read, transient), func(t *testing.T) {
				t.Parallel()
				synctest.Test(t, func(t *testing.T) {
					h := samplingHost{fakeHost: &fakeHost{}}
					calls := 0
					lost := func() bool {
						calls++
						return !transient || calls == 2
					}
					if read == "threads" {
						h.threads = func(pid int) ([]thread, error) {
							if lost() {
								return nil, os.ErrPermission
							}
							return h.fakeHost.Threads(pid)
						}
					} else {
						h.usage = func(pid int) (usage, error) {
							if lost() {
								return usage{}, errors.New("malformed usage")
							}
							return h.fakeHost.Usage(pid)
						}
					}
					r := New(fakeOptions(t, "work"))
					r.host = h
					spec := testSpec("sampling-loss", machine.R1, 275*time.Millisecond)
					trial, err := r.Start(context.Background(), spec)
					if err != nil {
						t.Fatal(err)
					}
					result, err := trial.Wait(context.Background(), &recorder{})
					if err != nil {
						t.Fatal(err)
					}
					if result.Inconclusive == "" {
						t.Fatalf("sampling loss allowed pass: %+v", result)
					}
					if diff := cmp.Diff(spec.Duration, result.Ran); diff != "" {
						t.Fatal(diff)
					}
					if diff := cmp.Diff(machine.Signal(""), result.Signal); diff != "" {
						t.Fatal(diff)
					}
				})
			})
		}
	}
}

func TestSamplingDisappearance(t *testing.T) {
	for _, read := range []string{"threads", "usage"} {
		t.Run(read, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				h := samplingHost{fakeHost: &fakeHost{}}
				if read == "threads" {
					h.threads = func(int) ([]thread, error) {
						return nil, fmt.Errorf("thread exited: %w", os.ErrNotExist)
					}
				} else {
					h.usage = func(int) (usage, error) {
						h.mu.Lock()
						p := h.procs[0]
						h.mu.Unlock()
						p.finish(nil)
						return usage{}, fmt.Errorf("process exited: %w", os.ErrNotExist)
					}
				}
				r := New(fakeOptions(t, "work"))
				r.host = h
				trial, err := r.Start(context.Background(), testSpec("disappearance", machine.R1, 275*time.Millisecond))
				if err != nil {
					t.Fatal(err)
				}
				result, err := trial.Wait(context.Background(), &recorder{})
				if err != nil {
					t.Fatal(err)
				}
				want := machine.Signal("")
				if read == "usage" {
					want = machine.UnexpectedExit
				}
				if diff := cmp.Diff(want, result.Signal); diff != "" {
					t.Fatal(diff)
				}
				if result.Inconclusive != "" {
					t.Fatalf("normal disappearance counted as loss: %+v", result)
				}
			})
		})
	}
}

func TestSamplingLossKeepsEscape(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		h := samplingHost{fakeHost: &fakeHost{}, threads: func(pid int) ([]thread, error) {
			return []thread{{TID: pid, CPU: 9}}, os.ErrPermission
		}}
		r := New(fakeOptions(t, "work"))
		r.host = h
		trial, err := r.Start(context.Background(), testSpec("partial-threads", machine.R1, time.Second))
		if err != nil {
			t.Fatal(err)
		}
		result, err := trial.Wait(context.Background(), &recorder{})
		if err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff([]int{9}, result.Escaped); diff != "" {
			t.Fatal(diff)
		}
	})
}

func TestSamplingLossKeepsBackendFailures(t *testing.T) {
	for _, tc := range []struct {
		name, mode string
		signal     machine.Signal
	}{
		{"computation", "error", machine.ComputationError},
		{"stall", "sleep", machine.Stall},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				h := samplingHost{fakeHost: &fakeHost{}, threads: func(int) ([]thread, error) {
					return nil, os.ErrPermission
				}}
				o := fakeOptions(t, tc.mode)
				if tc.signal == machine.Stall {
					o.StallGrace = 100 * time.Millisecond
					o.StallWindow = 150 * time.Millisecond
				}
				r := New(o)
				r.host = h
				trial, err := r.Start(context.Background(), testSpec("loss-with-failure", machine.R1, time.Second))
				if err != nil {
					t.Fatal(err)
				}
				result, err := trial.Wait(context.Background(), &recorder{})
				if err != nil {
					t.Fatal(err)
				}
				if diff := cmp.Diff(tc.signal, result.Signal); diff != "" {
					t.Fatal(diff)
				}
			})
		})
	}
}
