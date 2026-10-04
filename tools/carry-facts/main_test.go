package main

import (
	"bytes"
	"strings"
	"testing"

	"github.com/shgew/togi/internal/journal"
)

func TestTransitionSummaryTracksSoloLimitCheckState(t *testing.T) {
	for _, tc := range []struct {
		name  string
		clear journal.Payload
	}{
		{"backoff", &journal.TunerDecision{Core: 0, Phase: journal.PhaseSearch, Decision: journal.Backoff}},
		{"step deeper", &journal.TunerDecision{Core: 0, Phase: journal.PhaseSearch, Decision: journal.StepDeeper}},
		{"reset phase", &journal.CorePhase{Core: 0, From: journal.PhaseSearch, To: journal.PhaseSearch}},
		{"other phase", &journal.CorePhase{Core: 0, From: journal.PhaseHasRoom, To: journal.PhaseSearch}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var events []journal.Event
			add := func(p journal.Payload) {
				events = append(events, journal.Event{Seq: len(events) + 1, Data: p})
			}
			add(&journal.TunerDecision{Core: 0, Decision: journal.CheckSoloLimit})
			add(&journal.CorePhase{Core: 1, To: journal.PhaseSearch, CheckSoloLimit: true})
			add(&journal.TrialIntent{Core: new(0), Phase: journal.PhaseSearch})
			add(tc.clear)
			add(&journal.TrialIntent{Core: new(0), Phase: journal.PhaseSearch})
			add(&journal.TrialIntent{Core: new(1), Phase: journal.PhaseSearch})
			add(&journal.TunerDecision{Core: 0, Decision: journal.CheckSoloLimit})
			add(&journal.TrialIntent{Core: new(0), Phase: journal.PhaseSearch})
			demo := summarizeTransition(events)
			if demo.soloLimitTrials != 3 {
				t.Fatalf("live solo-limit-check trials = %d, want 3 (not the ordinary search start)", demo.soloLimitTrials)
			}
		})
	}
}

func TestTransitionSummaryCountsCarriedAnswerBeforeClearingSoloLimit(t *testing.T) {
	for _, start := range []journal.Payload{
		&journal.TunerDecision{Core: 0, Decision: journal.CheckSoloLimit},
		&journal.CorePhase{Core: 0, To: journal.PhaseSearch, CheckSoloLimit: true},
	} {
		events := []journal.Event{
			{Seq: 1, Data: &journal.TrialCarried{Outcome: journal.OutcomePass}},
			{Seq: 2, Data: start},
			{Seq: 3, Cause: []int{2, 1}, Data: &journal.CorePhase{Core: 0, From: journal.PhaseSearch, To: journal.PhaseHasRoom}},
			{Seq: 4, Data: &journal.CorePhase{Core: 0, From: journal.PhaseHasRoom, To: journal.PhaseSearch}},
			{Seq: 5, Data: &journal.TrialIntent{Core: new(0), Phase: journal.PhaseSearch}},
			{Seq: 6, Data: &journal.CheckingCycle{Cycle: 1, Event: journal.CycleEnd, Passed: true, Full: true}},
		}
		demo := summarizeTransition(events)
		if demo.answered != 1 || len(demo.decisions) != 1 || demo.decisions[0].Seq != 3 || demo.soloLimitTrials != 0 {
			t.Fatalf("carried solo-limit answer lost or check retained: %+v", demo)
		}
		var out bytes.Buffer
		if err := renderTransition(&out, demo); err != nil {
			t.Fatal(err)
		}
		if !strings.Contains(out.String(), "candidate-solo-limit completions citing carried passes: 1\nlive solo-limit-check trials: 0\n") {
			t.Fatalf("summary reports incorrect counts:\n%s", out.String())
		}
	}
}
