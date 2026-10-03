package session

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
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
	checks := map[string]bool{}
	for _, e := range s.o.journal.Events()[s.o.start:] {
		if p, ok := e.Data.(*journal.PreflightCheck); ok {
			if !p.OK {
				s.o.t.Fatal("sweep ran after failed preflight")
			}
			checks[p.Check] = true
		}
	}
	for _, name := range []string{"root", "cpu", "ryzen_smu", "readback", "slot_mapping", "backends", "systemd_run", "pm_table"} {
		if !checks[name] {
			s.o.t.Fatalf("sweep preceded nonwriting preflight check %s", name)
		}
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

func TestFailedSweepJournalFailureDoesNotWrite(t *testing.T) {
	for _, resume := range []bool{false, true} {
		t.Run(fmt.Sprintf("resume=%t", resume), func(t *testing.T) {
			cfg := small()
			cfg.BIOS = []int{-10, -20}
			in := simInput(t.TempDir(), newSim(t, cfg))
			if resume {
				sweepRun(t, in, false)
			}
			seams := in.Machine.Seams()
			boot, _ := seams.Host.BootID()
			j, err := journal.Open(in.Dir, journal.Options{Boot: boot, Now: in.Machine.Now, Monotonic: seams.Clock.Monotonic})
			if err != nil {
				t.Fatal(err)
			}
			defer j.Close()
			faulty := &failAppendJournal{Journal: wrapFor(in, nil)(j), check: "trial_scopes"}
			o := &sweepObservation{t: t, journal: faulty, start: len(faulty.Events()), fail: true}
			seams.SMU = sweepSMU{SMU: seams.SMU, o: o}
			seams.Trials = observedSweepTrials{Trials: seams.Trials, o: o}
			var stderr bytes.Buffer
			_, err = Run(context.Background(), Input{Config: in.Config, ConfigPath: in.ConfigPath, Boot: boot, Journal: faulty, Machine: seams, Stderr: &stderr})
			if !faulty.failed || !errors.Is(err, io.ErrClosedPipe) || o.swept || o.writes != 0 || o.trials != 0 {
				t.Fatalf("failed sweep record wrote offsets: journal failed=%t err=%v swept=%t writes=%d trials=%d", faulty.failed, err, o.swept, o.writes, o.trials)
			}
			for core, want := range cfg.BIOS {
				got, err := in.Machine.Seams().SMU.Offset(core)
				if err != nil || got != want {
					t.Fatalf("core %d changed after failed sweep: got=%d want=%d err=%v", core, got, want, err)
				}
			}
			t.Logf("failed sweep and failed result append: zero actual SMU writes and zero trials; %s", stderr.String())
		})
	}
}
