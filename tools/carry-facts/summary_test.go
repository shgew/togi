package main

import (
	"bytes"
	"errors"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestTransitionSummarySeparatesCarriedAndLiveEvidence(t *testing.T) {
	events := []journal.Event{
		{Seq: 1, Data: &journal.TrialCarried{Source: journal.FactSource{Session: "s1", Seq: 4, Trial: "1"}, Class: journal.TrialClass{Regime: machine.R7, Workload: "fixture", Cores: []int{0, 1}, DurationS: 90}, Condition: machine.Resident, Profile: []int{-10, -10}, Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 30}},
		{Seq: 2, Data: &journal.FailureCarried{Source: journal.FactSource{Session: "s1", Seq: 5}, Class: journal.TrialClass{Regime: machine.R6, Cores: []int{0, 1}}, Condition: machine.Resident, Profile: []int{-10, -10}, Signal: machine.Crash, Attribution: journal.Unattributed}},
		{Seq: 3, Data: &journal.Failure{Trial: "live", Condition: machine.Resident, Profile: []int{-15, -15}, Signal: machine.Crash, Attribution: journal.Unattributed}},
		{Seq: 4, Msg: "skip carried failure", Cause: []int{1}, Data: &journal.Failure{KnownFailure: 1}},
		{Seq: 5, Msg: "hunt carried failure", Cause: []int{1}, Data: &journal.HuntStart{Hunt: 1, Failure: 1, Trial: "1", Regime: machine.R7, Workload: "fixture", Cores: []int{0, 1}, DurationS: 90, Failing: []int{-10, -10}}},
		{Seq: 6, Msg: "infer carried failure", Cause: []int{5, 1}, Data: &journal.HuntMask{Hunt: 1, Mask: 1, Cores: []int{0, 1}, Profile: []int{-10, -10}, Inferred: "failure"}},
		{Seq: 7, Data: &journal.HuntEnd{Hunt: 1, Result: "cancelled"}},
		{Seq: 8, Data: &journal.HuntStart{Hunt: 2, Failure: 3, Trial: "live", Regime: machine.R7, Workload: "fixture", Cores: []int{0, 1}, DurationS: 90, Failing: []int{-15, -15}}},
		{Seq: 9, Cause: []int{8, 3}, Data: &journal.HuntMask{Hunt: 2, Mask: 1, Cores: []int{0, 1}, Profile: []int{-15, -15}, Inferred: "failure"}},
		{Seq: 10, Data: &journal.HuntEnd{Hunt: 2, Result: "cancelled"}},
		{Seq: 11, Msg: "guard R1 trial at [-5 -5]", Data: &journal.TrialIntent{Trial: "guard-pass", Regime: machine.R1, Workload: "fixture", Core: new(0), Profile: []int{-5, -5}, DurationS: 90, Condition: machine.Resident, Phase: journal.PhaseGuard, Rotation: 1}},
		{Seq: 12, Data: &journal.TrialEnd{Trial: "guard-pass", Outcome: journal.OutcomePass, DurationS: 90}},
		{Seq: 13, Msg: "guard R2 trial at [-5 -5]", Data: &journal.TrialIntent{Trial: "guard-fail", Regime: machine.R2, Workload: "fixture", Core: new(0), Profile: []int{-5, -5}, DurationS: 90, Condition: machine.Resident, Phase: journal.PhaseGuard, Rotation: 1}},
		{Seq: 14, Data: &journal.TrialEnd{Trial: "guard-fail", Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, DurationS: 30}},
		{Seq: 15, Data: &journal.GuardRotation{Rotation: 1, Event: journal.RotationEnd}},
		{Seq: 16, Msg: "guard R1 trial at [-4 -5]", Data: &journal.TrialIntent{Trial: "next-pass", Regime: machine.R1, Workload: "fixture", Core: new(0), Profile: []int{-4, -5}, DurationS: 90, Condition: machine.Resident, Phase: journal.PhaseGuard, Rotation: 2}},
		{Seq: 17, Data: &journal.TrialEnd{Trial: "next-pass", Outcome: journal.OutcomePass, DurationS: 90}},
		{Seq: 18, Msg: "clean qualifying rotation", Data: &journal.GuardRotation{Rotation: 2, Event: journal.RotationEnd, Clean: true, Qualifying: true}},
	}
	demo := summarizeTransition(events)
	want := transitionDemo{failures: 2, rotationStarts: 2, rotationPasses: 1, firstTrials: []journal.Event{events[10], events[12], events[15]}, skips: events[3:4], hunts: events[4:5], inferredMasks: events[5:6], firstRotation: &events[17]}
	if diff := cmp.Diff(want, demo, cmp.AllowUnexported(transitionDemo{})); diff != "" {
		t.Fatalf("summary (-want +got):\n%s", diff)
	}
	var output bytes.Buffer
	if err := renderTransition(&output, demo); err != nil {
		t.Fatal(err)
	}
	golden, err := os.ReadFile(filepath.Join("testdata", "transition.golden"))
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(string(golden), output.String()); diff != "" {
		t.Fatalf("render (-want +got):\n%s", diff)
	}
}

type failingOutput struct{ err error }

func (w failingOutput) Write([]byte) (int, error) { return 0, w.err }

func TestRenderTransitionRequiresQualifyingRotationAndPropagatesWriteError(t *testing.T) {
	var output bytes.Buffer
	if err := renderTransition(&output, transitionDemo{}); err == nil {
		t.Fatal("reported success without a qualifying rotation")
	}
	failure := errors.New("output unavailable")
	if err := renderTransition(failingOutput{failure}, transitionDemo{}); !errors.Is(err, failure) {
		t.Fatalf("render error=%v, want %v", err, failure)
	}
}
