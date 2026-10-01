package trial

import (
	"context"
	"errors"
	"io"
	"os"
	"syscall"
	"testing"
	"testing/synctest"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/backend/mprime"
	"github.com/shgew/togi/internal/machine"
)

type writerEvidence struct {
	Core   int
	Signal machine.Signal
	Detail string
}

type writerEvidenceReport struct {
	recorder
	evidence []writerEvidence
}

func (r *writerEvidenceReport) Signal(core int, signal machine.Signal, detail string) {
	r.evidence = append(r.evidence, writerEvidence{core, signal, detail})
}

type writerEvidenceHost struct {
	*outputHost
	killGroup func(process) error
}

func (h *writerEvidenceHost) SignalGroup(p process, sig syscall.Signal) error {
	if sig == syscall.SIGKILL && h.killGroup != nil {
		if err := h.killGroup(p); err != nil {
			return err
		}
	}
	return h.outputHost.SignalGroup(p, sig)
}

func appendWriterEvidence(t *testing.T, path, text string) {
	t.Helper()
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND, 0)
	if err != nil {
		t.Fatal(err)
	}
	_, writeErr := io.WriteString(f, text)
	closeErr := f.Close()
	if writeErr != nil || closeErr != nil {
		t.Fatalf("append descendant output: write=%v close=%v", writeErr, closeErr)
	}
}

func TestTeardownPreservesIndependentWriterEvidence(t *testing.T) {
	for _, failure := range []string{"scope kill", "group kill"} {
		for _, confirmed := range []string{"killed scope", "missing scope"} {
			t.Run(failure+"/"+confirmed, func(t *testing.T) {
				synctest.Test(t, func(t *testing.T) {
					o := fakeOptions(t, "watched")
					o.NoScope = false
					h := &writerEvidenceHost{outputHost: &outputHost{fakeHost: &fakeHost{inScope: true}, text: "FATAL ", watch: true}}
					r := New(o)
					r.host = h
					spec := testSpec("independent-writers", machine.R7, 20*time.Millisecond)
					spec.Cores, spec.CPUs = []int{0, 1}, []int{0, 1}
					started, err := r.Start(context.Background(), spec)
					if err != nil {
						t.Fatal(err)
					}
					trial := started.(*running)
					trial.backend = mprime.New("")
					unconfirmed, stopped := trial.instances[0], trial.instances[1]
					killErr := errors.New("injected owned-writer cleanup failure")
					h.killScope = func(scope string) ([]byte, error) {
						if scope == unconfirmed.Scope {
							appendWriterEvidence(t, unconfirmed.watch[0].path, "ERROR: unconfirmed descendant")
							if failure == "scope kill" {
								return nil, killErr
							}
							return nil, nil
						}
						appendWriterEvidence(t, stopped.watch[0].path, "ERROR: confirmed descendant")
						if confirmed == "missing scope" {
							return []byte("Unit could not be found"), errors.New("exit status 1")
						}
						return nil, nil
					}
					h.killGroup = func(p process) error {
						if failure == "group kill" && p == unconfirmed.process {
							return killErr
						}
						// Grace has reaped the launchers. Scope cleanup, not launcher
						// disappearance, confirms the surviving descendant writers.
						return syscall.ESRCH
					}
					var report writerEvidenceReport
					begin := time.Now()
					result, err := trial.Wait(context.Background(), &report)
					if !errors.Is(err, machine.ErrContainment) || !errors.Is(err, killErr) {
						t.Fatalf("unconfirmed cleanup must remain a containment dead end: result=%+v err=%v", result, err)
					}
					if result.Signal != machine.ComputationError || result.Core != stopped.Core || result.Inconclusive != "" || len(result.Escaped) != 0 {
						t.Fatalf("unrelated cleanup failure lost confirmed computation evidence: result=%+v err=%v", result, err)
					}
					want := []writerEvidence{{Core: stopped.Core, Signal: machine.ComputationError, Detail: "FATAL ERROR: confirmed descendant"}}
					if diff := cmp.Diff(want, report.evidence); diff != "" {
						t.Fatalf("independent final evidence (-want +got):\n%s", diff)
					}
					if elapsed := time.Since(begin); elapsed > spec.Duration+teardownLimit {
						t.Fatalf("independent writer cleanup exceeded shared deadline: %s", elapsed)
					}
					if again := trial.teardown(&result, &report); !errors.Is(again, machine.ErrContainment) || !errors.Is(again, killErr) {
						t.Fatalf("repeated teardown lost containment failure: %v", again)
					}
					if diff := cmp.Diff(want, report.evidence); diff != "" {
						t.Fatalf("teardown reported final evidence more than once (-want +got):\n%s", diff)
					}
				})
			})
		}
	}
}

func TestNoScopeWatchedPartialRequiresConfirmedGroupKill(t *testing.T) {
	killErr := errors.New("injected owned-group cleanup failure")
	for _, tt := range []struct {
		name string
		err  error
	}{
		{"verified group kill", nil},
		{"launcher gone", syscall.ESRCH},
		{"group kill failed", killErr},
	} {
		t.Run(tt.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				o := fakeOptions(t, "watched")
				h := &writerEvidenceHost{outputHost: &outputHost{fakeHost: &fakeHost{}, text: "FATAL ", watch: true}}
				r := New(o)
				r.host = h
				started, err := r.Start(context.Background(), testSpec("owned-group-writers", machine.R1, 20*time.Millisecond))
				if err != nil {
					t.Fatal(err)
				}
				trial := started.(*running)
				trial.backend = mprime.New("")
				inst := trial.instances[0]
				h.killGroup = func(process) error {
					appendWriterEvidence(t, inst.watch[0].path, "ERROR: descendant after launcher exit")
					return tt.err
				}
				var report writerEvidenceReport
				result, err := trial.Wait(context.Background(), &report)
				var want []writerEvidence
				if tt.err == nil {
					if err != nil || result.Signal != machine.ComputationError || result.Inconclusive != "" {
						t.Fatalf("verified owned-group final evidence: result=%+v err=%v", result, err)
					}
					want = []writerEvidence{{Core: inst.Core, Signal: machine.ComputationError, Detail: "FATAL ERROR: descendant after launcher exit"}}
				} else {
					if !errors.Is(err, machine.ErrContainment) || !errors.Is(err, errOutputDrainUnconfirmed) || result.Signal != "" || result.Inconclusive != "" {
						t.Fatalf("unconfirmed descendants became final evidence: result=%+v err=%v", result, err)
					}
					if errors.Is(tt.err, killErr) && !errors.Is(err, killErr) {
						t.Fatalf("owned-group cleanup failure lost: %v", err)
					}
				}
				if diff := cmp.Diff(want, report.evidence); diff != "" {
					t.Fatalf("owned-group final evidence (-want +got):\n%s", diff)
				}
			})
		})
	}
}
