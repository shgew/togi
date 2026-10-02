package main

import (
	"bytes"
	"strings"
	"testing"

	"github.com/shgew/togi/internal/journal"
)

func TestTransitionSummaryTracksEdgeCheckState(t *testing.T) {
	for _, tc := range []struct {
		name  string
		clear journal.Payload
	}{
		{"backoff", &journal.TunerDecision{Core: 0, Phase: journal.PhaseSearch, Decision: journal.Backoff}},
		{"step deeper", &journal.TunerDecision{Core: 0, Phase: journal.PhaseSearch, Decision: journal.StepDeeper}},
		{"reset phase", &journal.CorePhase{Core: 0, From: journal.PhaseSearch, To: journal.PhaseSearch}},
		{"other phase", &journal.CorePhase{Core: 0, From: journal.PhaseResident, To: journal.PhaseSearch}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var events []journal.Event
			add := func(p journal.Payload) {
				events = append(events, journal.Event{Seq: len(events) + 1, Data: p})
			}
			add(&journal.TunerDecision{Core: 0, Decision: journal.CheckEdge})
			add(&journal.CorePhase{Core: 1, To: journal.PhaseSearch, CheckEdge: true})
			add(&journal.TrialIntent{Core: new(0), Phase: journal.PhaseSearch})
			add(tc.clear)
			add(&journal.TrialIntent{Core: new(0), Phase: journal.PhaseSearch})
			add(&journal.TrialIntent{Core: new(1), Phase: journal.PhaseSearch})
			add(&journal.TunerDecision{Core: 0, Decision: journal.CheckEdge})
			add(&journal.TrialIntent{Core: new(0), Phase: journal.PhaseSearch})
			demo := summarizeTransition(events)
			if demo.edgeStarts != 3 {
				t.Fatalf("live edge-check starts = %d, want 3 (not the ordinary search start)", demo.edgeStarts)
			}
		})
	}
}

func TestTransitionSummaryCountsCarriedAnswerBeforeClearingEdge(t *testing.T) {
	for _, start := range []journal.Payload{
		&journal.TunerDecision{Core: 0, Decision: journal.CheckEdge},
		&journal.CorePhase{Core: 0, To: journal.PhaseSearch, CheckEdge: true},
	} {
		events := []journal.Event{
			{Seq: 1, Data: &journal.TrialCarried{Outcome: journal.OutcomePass}},
			{Seq: 2, Data: start},
			{Seq: 3, Cause: []int{2, 1}, Data: &journal.CorePhase{Core: 0, From: journal.PhaseSearch, To: journal.PhaseResident}},
			{Seq: 4, Data: &journal.CorePhase{Core: 0, From: journal.PhaseResident, To: journal.PhaseSearch}},
			{Seq: 5, Data: &journal.TrialIntent{Core: new(0), Phase: journal.PhaseSearch}},
			{Seq: 6, Data: &journal.GuardRotation{Rotation: 1, Event: journal.RotationEnd, Clean: true, Qualifying: true}},
		}
		demo := summarizeTransition(events)
		if demo.answered != 1 || len(demo.decisions) != 1 || demo.decisions[0].Seq != 3 || demo.edgeStarts != 0 {
			t.Fatalf("carried edge answer lost or check retained: %+v", demo)
		}
		var out bytes.Buffer
		if err := renderTransition(&out, demo); err != nil {
			t.Fatal(err)
		}
		if !strings.Contains(out.String(), "candidate-edge completions citing carried passes: 1\nlive edge-check starts: 0\n") {
			t.Fatalf("summary reports incorrect counts:\n%s", out.String())
		}
	}
}
