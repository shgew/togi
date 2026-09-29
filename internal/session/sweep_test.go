package session

import (
	"context"
	"errors"
	"fmt"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type sweepObservation struct {
	t              *testing.T
	journal        Journal
	start          int
	swept          bool
	writes, trials int
	fail           bool
	cancel         context.CancelFunc
}

type sweepSMU struct {
	machine.SMU
	o *sweepObservation
}

func (s sweepSMU) SetOffset(core, offset int) error {
	if !s.o.swept {
		s.o.t.Fatal("SMU write before successful leftover-scope sweep")
	}
	s.o.writes++
	return s.SMU.SetOffset(core, offset)
}
func (s sweepSMU) SetAllOffsets(offset int) error {
	if !s.o.swept {
		s.o.t.Fatal("all-core SMU write before successful leftover-scope sweep")
	}
	s.o.writes++
	return s.SMU.SetAllOffsets(offset)
}

type observedSweepTrials struct {
	machine.Trials
	o *sweepObservation
}

func (s observedSweepTrials) Sweep(ctx context.Context) (string, error) {
	if s.o.writes != 0 || s.o.trials != 0 {
		s.o.t.Fatal("sweep followed a write or trial")
	}
	checks := 0
	for _, e := range s.o.journal.Events()[s.o.start:] {
		if p, ok := e.Data.(*journal.PreflightCheck); ok {
			if !p.OK {
				s.o.t.Fatal("sweep ran after failed preflight")
			}
			checks++
		}
	}
	if checks < 7 {
		s.o.t.Fatalf("sweep preceded nonwriting preflight: %d checks", checks)
	}
	if s.o.fail {
		return "", errors.New("stale scope cleanup cannot be confirmed")
	}
	detail, err := s.Trials.Sweep(ctx)
	s.o.swept = err == nil
	return detail, err
}

func (s observedSweepTrials) Start(ctx context.Context, spec machine.TrialSpec) (machine.Running, error) {
	if !s.o.swept {
		s.o.t.Fatal("trial before successful leftover-scope sweep")
	}
	s.o.trials++
	r, err := s.Trials.Start(ctx, spec)
	s.o.cancel()
	return r, err
}

func sweepRun(t *testing.T, in simRun, fail bool) (Stop, *sweepObservation) {
	t.Helper()
	seams := in.Machine.Seams()
	boot, _ := seams.Host.BootID()
	j, err := journal.Open(in.Dir, journal.Options{Boot: boot, Now: in.Machine.Now, Monotonic: seams.Clock.Monotonic})
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	jr := wrapFor(in, nil)(j)
	o := &sweepObservation{t: t, journal: jr, start: len(jr.Events()), fail: fail, cancel: cancel}
	seams.SMU = sweepSMU{SMU: seams.SMU, o: o}
	seams.Trials = observedSweepTrials{Trials: seams.Trials, o: o}
	stop, err := Run(ctx, Input{Config: in.Config, ConfigPath: in.ConfigPath, Boot: boot, Journal: jr, Machine: seams, Bootloader: in.Bootloader})
	if err != nil {
		t.Fatal(err)
	}
	return stop, o
}

func TestSweepBeforeWritesStartAndResume(t *testing.T) {
	for _, mode := range []string{"in-session", "tuning boot"} {
		for _, startup := range []string{"fresh", "same-boot resume", "reboot resume"} {
			for _, fail := range []bool{false, true} {
				t.Run(fmt.Sprintf("%s/%s/fail=%t", mode, startup, fail), func(t *testing.T) {
					in := simInput(t.TempDir(), newSim(t, small()))
					if mode == "tuning boot" {
						in.Bootloader = &fakeBootloader{}
					}
					if startup != "fresh" {
						stop, _ := sweepRun(t, in, false)
						if stop.Reason != StopSignal {
							t.Fatalf("setup stopped: %+v", stop)
						}
						if startup == "reboot resume" {
							in.Machine.Reboot()
						}
					}
					stop, o := sweepRun(t, in, fail)
					if fail {
						if stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != journal.DeadEndContainment {
							t.Fatalf("failed sweep stop: %+v", stop)
						}
						if diff := cmp.Diff([]int{0, 0}, []int{o.writes, o.trials}); diff != "" {
							t.Fatalf("failure writes/trials (-want +got):\n%s", diff)
						}
						if len(stop.Evidence) != 1 || stop.Evidence[0].Kind != journal.KindPreflightCheck {
							t.Fatalf("sweep failure evidence: %+v", stop.Evidence)
						}
						for _, e := range o.journal.Events()[o.start:] {
							if e.Kind == journal.KindSMUWrite || e.Kind == journal.KindTrialIntent {
								t.Fatalf("failed sweep recorded %s", e.Kind)
							}
						}
						t.Log("failed sweep: containment dead end; zero actual SMU writes and zero trials")
					} else {
						if stop.Reason != StopSignal || o.writes == 0 || o.trials != 1 {
							t.Fatalf("successful sweep stopped %+v; writes=%d trials=%d", stop, o.writes, o.trials)
						}
						t.Logf("preflight -> sweep -> %d actual SMU writes; one trial then clean stop", o.writes)
					}
				})
			}
		}
	}
}

func TestFailedPreflightSkipsSweep(t *testing.T) {
	in := simInput(t.TempDir(), newSim(t, small()))
	in.Machine.FailCheck("root", "not root")
	stop, o := sweepRun(t, in, false)
	if stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != journal.DeadEndPreflight || o.swept || o.writes != 0 || o.trials != 0 {
		t.Fatalf("preflight stop %+v; observation %+v", stop, o)
	}
}
