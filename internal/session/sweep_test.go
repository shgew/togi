package session

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/trialfiles"
)

type sweepObservation struct {
	t              *testing.T
	journal        Journal
	start          int
	swept          bool
	writes, trials int
	fail           bool
	ended          []string
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

// Ended stands for removing a trial's plain samples and backend logs: only a successful sweep proves that no stale scope
// still writes them, and the durable trial.end must follow, so a run killed in between retries the compression.
func (s observedSweepTrials) Ended(id string) error {
	if !s.o.swept {
		s.o.t.Errorf("trial %s files finalized before a successful leftover-scope sweep", id)
	}
	for _, e := range s.o.journal.Events() {
		if p, ok := e.Data.(*journal.TrialEnd); ok && p.Trial == id {
			s.o.t.Errorf("trial %s files finalized after its durable trial.end at seq %d", id, e.Seq)
		}
	}
	s.o.ended = append(s.o.ended, id)
	return s.Trials.Ended(id)
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

// TestSameBootOpenTrialFinalizesAfterSweep leaves a trial open in the current boot, as an owner that died with its
// scope alive does, beside samples and a backend log that scope may still write. Recovery closes and compresses that
// trial only after the nonwriting preflight and the stale-scope sweep succeed; a failed check leaves it open with its
// plain files readable, so no compression can race a live writer.
func TestSameBootOpenTrialFinalizesAfterSweep(t *testing.T) {
	for _, tc := range []struct {
		name            string
		preflight, fail bool
	}{
		{"swept", false, false},
		{"failed preflight", true, false},
		{"failed sweep", false, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			in, _, id, plain := openSameBootTrial(t)
			if tc.preflight {
				in.Machine.FailCheck("root", "not root")
			}
			stop, o := sweepRun(t, in, tc.fail)
			var swept, ended int
			for _, e := range o.journal.Events()[o.start:] {
				switch p := e.Data.(type) {
				case *journal.PreflightCheck:
					if p.Check == "trial_scopes" && p.OK {
						swept = e.Seq
					}
				case *journal.TrialEnd:
					if p.Trial == id {
						ended = e.Seq
					}
				}
			}
			if tc.preflight || tc.fail {
				want := journal.DeadEndContainment
				if tc.preflight {
					want = journal.DeadEndPreflight
				}
				if stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != want {
					t.Fatalf("failed check stop: %+v", stop)
				}
				if len(o.ended) != 0 || ended != 0 {
					t.Fatalf("unsafe open trial %s finalized: Ended calls %v, trial.end seq %d", id, o.ended, ended)
				}
				for path, content := range plain {
					got, err := os.ReadFile(path)
					if err != nil || string(got) != content {
						t.Fatalf("plain %s after failed check: %q, %v", path, got, err)
					}
					if _, err := os.Stat(path + ".gz"); !os.IsNotExist(err) {
						t.Fatalf("compressed %s after failed check: %v", path, err)
					}
				}
				// The pending dead end resumes through the same boundary: the next successful sweep closes the trial first.
				in.Machine.FailCheck("root", "")
				stop, retry := sweepRun(t, in, false)
				if stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != want {
					t.Fatalf("retry stop: %+v", stop)
				}
				if len(retry.ended) != 1 || retry.ended[0] != id {
					t.Fatalf("retry finalized %v, want [%s]", retry.ended, id)
				}
				var sweptOK, trialEnd, shutdown int
				for _, e := range retry.journal.Events()[retry.start:] {
					switch p := e.Data.(type) {
					case *journal.PreflightCheck:
						if p.Check == "trial_scopes" && p.OK {
							sweptOK = e.Seq
						}
					case *journal.TrialEnd:
						if p.Trial == id {
							trialEnd = e.Seq
						}
					case *journal.Shutdown:
						shutdown = e.Seq
					}
				}
				if sweptOK == 0 || trialEnd < sweptOK || (shutdown != 0 && shutdown < trialEnd) {
					t.Fatalf("retry order: sweep %d, trial.end %d, shutdown %d", sweptOK, trialEnd, shutdown)
				}
				return
			}
			if stop.Reason != StopSignal || swept == 0 || ended < swept || len(o.ended) == 0 || o.ended[0] != id {
				t.Fatalf("recovered trial %s: stop %+v, sweep seq %d, trial.end seq %d, Ended calls %v", id, stop, swept, ended, o.ended)
			}
			for path, content := range plain {
				if _, err := os.Stat(path); !os.IsNotExist(err) {
					t.Fatalf("plain %s kept after recovered finalization: %v", path, err)
				}
				f, err := trialfiles.Open(path)
				if err != nil {
					t.Fatalf("open compressed %s: %v", path, err)
				}
				got, err := io.ReadAll(f)
				f.Close()
				if err != nil || string(got) != content {
					t.Fatalf("compressed %s: %q, %v", path, got, err)
				}
			}
		})
	}
}

// openSameBootTrial stops a run right after its first trial.start, in the same boot, and leaves plain samples and a
// backend log in that trial's directory as the surviving scope would.
func openSameBootTrial(t *testing.T) (in simRun, trials, id string, plain map[string]string) {
	t.Helper()
	in = simInput(t.TempDir(), newSim(t, small()))
	trials = filepath.Join(in.Dir, "trials")
	in.Machine.SetSamplesDir(trials)
	gate := &appendGate{after: true, match: eventKind(journal.KindTrialStart), do: func(journal.Event) error { return errKilled }}
	if _, err := simulateBoot(context.Background(), in, wrapFor(in, gate)); !errors.Is(err, errKilled) || !gate.fired {
		t.Fatalf("setup did not stop inside the first trial: %v", err)
	}
	for _, e := range readEvents(t, in.Dir) {
		switch p := e.Data.(type) {
		case *journal.TrialStart:
			id = p.Trial
		case *journal.TrialEnd:
			t.Fatalf("setup ended trial %s", p.Trial)
		}
	}
	if id == "" {
		t.Fatal("setup started no trial")
	}
	plain = map[string]string{}
	plain[filepath.Join(trials, id, trialfiles.Samples)] = "{\"elapsed_ms\":1000}\n"
	plain[filepath.Join(trials, id, "c00", "stdout.log")] = "backend still running\n"
	for path, content := range plain {
		if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(content), 0644); err != nil {
			t.Fatal(err)
		}
	}
	return in, trials, id, plain
}

type countedEndTrials struct {
	machine.Trials
	ended *[]string
}

func (c countedEndTrials) Ended(id string) error {
	*c.ended = append(*c.ended, id)
	return c.Trials.Ended(id)
}

// TestRecoveredFinalizationRetriesAfterInterruptedEnd stops the recovering run after the files were compressed and
// before its trial.end became durable. The next run compresses again, as a no-op, and closes the trial exactly once.
func TestRecoveredFinalizationRetriesAfterInterruptedEnd(t *testing.T) {
	in, _, id, plain := openSameBootTrial(t)
	var first []string
	seams := in.Machine.Seams()
	seams.Trials = countedEndTrials{Trials: seams.Trials, ended: &first}
	in.Seams = &seams
	gate := &appendGate{match: eventKind(journal.KindTrialEnd), do: func(journal.Event) error { return errKilled }}
	if _, err := simulateBoot(context.Background(), in, wrapFor(in, gate)); !errors.Is(err, errKilled) || !gate.fired {
		t.Fatalf("interruption before the durable trial.end: %v", err)
	}
	if len(first) != 1 || first[0] != id {
		t.Fatalf("first run finalized %v, want [%s]", first, id)
	}
	for _, e := range readEvents(t, in.Dir) {
		if p, ok := e.Data.(*journal.TrialEnd); ok && p.Trial == id {
			t.Fatalf("trial.end durable despite the interruption: %+v", e)
		}
	}
	for path := range plain {
		if _, err := os.Stat(path); !os.IsNotExist(err) {
			t.Fatalf("plain %s kept after finalization: %v", path, err)
		}
		if _, err := os.Stat(path + ".gz"); err != nil {
			t.Fatalf("compressed %s missing: %v", path, err)
		}
	}
	in.Seams = nil
	stop, o := sweepRun(t, in, false)
	if stop.Reason != StopSignal || len(o.ended) == 0 || o.ended[0] != id {
		t.Fatalf("retry stop %+v, Ended calls %v", stop, o.ended)
	}
	ends := 0
	for _, e := range readEvents(t, in.Dir) {
		if p, ok := e.Data.(*journal.TrialEnd); ok && p.Trial == id {
			ends++
		}
	}
	if ends != 1 {
		t.Fatalf("trial %s closed %d times", id, ends)
	}
	for path, content := range plain {
		f, err := trialfiles.Open(path)
		if err != nil {
			t.Fatalf("open %s: %v", path, err)
		}
		got, err := io.ReadAll(f)
		f.Close()
		if err != nil || string(got) != content {
			t.Fatalf("%s: %q, %v", path, got, err)
		}
	}
}
