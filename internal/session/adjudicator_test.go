package session

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestAdjudicatorPrecedence(t *testing.T) {
	rows := []struct {
		name    string
		apply   func(*trialEvidence)
		outcome journal.Outcome
		signal  machine.Signal
	}{
		{"escape", func(e *trialEvidence) { e.result.Escaped = []int{99} }, journal.OutcomeInconclusive, ""},
		{"backend", func(e *trialEvidence) { e.result.Signal = machine.ComputationError }, journal.OutcomeFailure, machine.ComputationError},
		{"mce", func(e *trialEvidence) { e.mces = []recordedMCE{{corrected: true}} }, journal.OutcomeFailure, machine.CorrectedMCE},
		{"reset", func(e *trialEvidence) {
			e.reset = &journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash}
		}, journal.OutcomeFailure, machine.Crash},
		{"missing", func(e *trialEvidence) { e.missing = "sampling unavailable" }, journal.OutcomeInconclusive, ""},
		{"pass", func(*trialEvidence) {}, journal.OutcomePass, ""},
	}
	for i, upper := range rows {
		for _, lower := range rows[i:] {
			t.Run(upper.name+"/"+lower.name, func(t *testing.T) {
				e := trialEvidence{result: machine.Result{Ran: time.Minute}, duration: time.Minute}
				lower.apply(&e)
				upper.apply(&e)
				got := adjudicateTrial(e)
				if diff := cmp.Diff(upper.outcome, got.Outcome); diff != "" {
					t.Fatal(diff)
				}
				if diff := cmp.Diff(upper.signal, got.Signal); diff != "" {
					t.Fatal(diff)
				}
			})
		}
	}
}

func TestBackendFailuresAndDuration(t *testing.T) {
	for _, signal := range []machine.Signal{machine.ComputationError, machine.Stall, machine.UnexpectedExit} {
		e := trialEvidence{result: machine.Result{Signal: signal}, missing: "canceled; cleanup failed", reset: &journal.TrialEnd{Outcome: journal.OutcomeInconclusive}, mces: []recordedMCE{{corrected: true}}}
		end := adjudicateTrial(e)
		if diff := cmp.Diff(signal, end.Signal); diff != "" {
			t.Fatal(diff)
		}
		if end.Outcome != journal.OutcomeFailure || end.Reason != e.missing {
			t.Fatalf("lost backend evidence: %+v", end)
		}
	}
	for _, ran := range []time.Duration{time.Minute - time.Nanosecond, time.Minute, time.Minute + time.Nanosecond} {
		end := adjudicateTrial(trialEvidence{result: machine.Result{Ran: ran}, duration: time.Minute})
		want := journal.OutcomePass
		if ran < time.Minute {
			want = journal.OutcomeInconclusive
		}
		if diff := cmp.Diff(want, end.Outcome); diff != "" {
			t.Fatal(diff)
		}
	}
}

type evidenceTrials struct {
	machine.Trials
	result machine.Result
	err    error
}

func (t evidenceTrials) Start(ctx context.Context, s machine.TrialSpec) (machine.Running, error) {
	r, err := t.Trials.Start(ctx, s)
	if err != nil {
		return nil, err
	}
	return evidenceRunning{Running: r, result: t.result, err: t.err}, nil
}

type evidenceRunning struct {
	machine.Running
	result machine.Result
	err    error
}

func (r evidenceRunning) Wait(context.Context, machine.Reporter) (machine.Result, error) {
	return r.result, r.err
}

type evidenceKernel struct{ machine.Kernel }

func (k evidenceKernel) MCEs(string, time.Duration) ([]machine.MCE, error) {
	return []machine.MCE{{Core: 0, Corrected: true, Lines: []string{"test machine check"}}}, errors.New("kernel read failed")
}

func TestRunnerEvidenceSurvivesErrors(t *testing.T) {
	for _, tc := range []struct {
		name    string
		result  machine.Result
		kernel  bool
		outcome journal.Outcome
		signal  machine.Signal
	}{
		{"signal", machine.Result{Signal: machine.ComputationError}, false, journal.OutcomeFailure, machine.ComputationError},
		{"escape", machine.Result{Escaped: []int{99}}, false, journal.OutcomeInconclusive, ""},
		{"mce", machine.Result{}, true, journal.OutcomeFailure, machine.CorrectedMCE},
	} {
		t.Run(tc.name, func(t *testing.T) {
			in := simInput(t.TempDir(), newSim(t, small()))
			seams := in.Machine.Seams()
			seams.Trials = evidenceTrials{Trials: seams.Trials, result: tc.result, err: errors.New("cleanup failed")}
			if tc.kernel {
				seams.Kernel = evidenceKernel{seams.Kernel}
			}
			_, err := runWithSeams(context.Background(), in, seams)
			if err != nil {
				t.Fatal(err)
			}
			for _, ev := range readEvents(t, in.Dir) {
				if end, ok := ev.Data.(*journal.TrialEnd); ok {
					if diff := cmp.Diff(tc.outcome, end.Outcome); diff != "" {
						t.Fatal(diff)
					}
					if diff := cmp.Diff(tc.signal, end.Signal); diff != "" {
						t.Fatal(diff)
					}
					if !strings.Contains(end.Reason, "cleanup failed") {
						t.Fatalf("lost diagnostic: %+v", end)
					}
					return
				}
			}
			t.Fatal("no trial end")
		})
	}
}

func TestInterruptedBackendFailureWithoutReset(t *testing.T) {
	r, _, closeJournal := checkedRunner(t, []int{0, 0})
	defer closeJournal()
	for _, p := range []journal.Payload{
		&journal.TrialIntent{Trial: "0001", Core: new(0), Profile: []int{-10, 0}},
		&journal.TrialProgress{Trial: "0001", Signal: machine.ComputationError, Core: new(0)},
	} {
		if _, err := r.append(p); err != nil {
			t.Fatal(err)
		}
	}
	if err := r.closeOpenTrial(); err != nil {
		t.Fatal(err)
	}
	events := r.in.Journal.Events()
	end := events[len(events)-1].Data.(*journal.TrialEnd)
	if end.Outcome != journal.OutcomeFailure || end.Signal != machine.ComputationError || !end.Interrupted || end.Reason != journal.TrialReasonStoppedDuringTrial {
		t.Fatalf("interrupted backend failure without a reset: %+v", end)
	}
	t.Logf("same-boot interruption: outcome=%s signal=%s reason=%q", end.Outcome, end.Signal, end.Reason)
}

type missingBootKernel struct {
	machine.Kernel
	mces []machine.MCE
}

func (k missingBootKernel) MCEs(boot string, _ time.Duration) ([]machine.MCE, error) {
	return k.mces, fmt.Errorf("read kernel log of boot %s: %w", boot, machine.ErrBootMissing)
}

func TestRunnerMissingCurrentBoot(t *testing.T) {
	for _, tc := range []struct {
		name    string
		signal  machine.Signal
		mces    []machine.MCE
		outcome journal.Outcome
		want    machine.Signal
	}{
		{"missing", "", nil, journal.OutcomeInconclusive, ""},
		{"computation error", machine.ComputationError, nil, journal.OutcomeFailure, machine.ComputationError},
		{"stall", machine.Stall, nil, journal.OutcomeFailure, machine.Stall},
		{"early exit", machine.UnexpectedExit, nil, journal.OutcomeFailure, machine.UnexpectedExit},
		{"mce", "", []machine.MCE{{Core: 0, Corrected: true, Lines: []string{"test machine check"}}}, journal.OutcomeFailure, machine.CorrectedMCE},
	} {
		t.Run(tc.name, func(t *testing.T) {
			in := simInput(t.TempDir(), newSim(t, small()))
			seams := in.Machine.Seams()
			seams.Trials = evidenceTrials{Trials: seams.Trials, result: machine.Result{Ran: 24 * time.Hour, Signal: tc.signal}}
			seams.Kernel = missingBootKernel{Kernel: seams.Kernel, mces: tc.mces}
			if _, err := runWithSeams(context.Background(), in, seams); err != nil {
				t.Fatal(err)
			}
			for _, ev := range readEvents(t, in.Dir) {
				if end, ok := ev.Data.(*journal.TrialEnd); ok {
					if diff := cmp.Diff(tc.outcome, end.Outcome); diff != "" {
						t.Fatal(diff)
					}
					if diff := cmp.Diff(tc.want, end.Signal); diff != "" {
						t.Fatal(diff)
					}
					t.Logf("current boot missing: outcome=%s signal=%q", end.Outcome, end.Signal)
					return
				}
			}
			t.Fatal("no trial end")
		})
	}
}
