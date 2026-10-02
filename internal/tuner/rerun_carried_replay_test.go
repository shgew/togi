package tuner

import (
	"fmt"
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestCarriedRerunCitationReplay(t *testing.T) {
	for _, refine := range []bool{false, true} {
		t.Run(fmt.Sprintf("refine=%t", refine), func(t *testing.T) {
			h := newHarness(t, coreStart{phase: journal.PhaseDone, offset: -10, fail: new(-11)})
			cfg := config.Default()
			cfg.Guard.Rotation = []machine.Regime{machine.R1}
			h.add(&journal.ConfigLoaded{Config: snapshotConfig(cfg)})
			h.add(&journal.ProfileChange{To: []int{-10}})
			failure := carryTrials(h, machine.R1, []int{0}, []int{-10}, 900, 1, journal.OutcomeFailure)[0]
			facts := carryTrials(h, machine.R1, []int{0}, []int{-10}, h.s.durations.StartS, h.s.n, journal.OutcomePass)
			facts = append(facts, carryTrials(h, machine.R1, []int{0}, []int{-10}, 900, 1, journal.OutcomePass)...)
			if refine {
				h.add(&journal.RefineRound{Round: 1, Event: journal.RotationStart, Profile: []int{-9}, Target: []int{-9}, Cores: []int{0}, Starts: h.s.n, StartS: h.s.durations.StartS})
			}
			h.add(&journal.TunerDecision{Core: 0, Phase: journal.PhaseGuard, Decision: journal.Backoff, FromOffset: -10, ToOffset: -9, FailedMark: new(-10)}, failure)
			h.add(&journal.ProfileChange{From: []int{-10}, To: []int{-9}})
			a := h.next()
			if refine {
				if p, ok := a.Payload.(*journal.RefineRound); !ok || p.Event != journal.RotationEnd || !p.Passed {
					t.Fatalf("rerun did not finish refinement: %+v", a)
				}
			} else if p, ok := a.Payload.(*journal.GuardRotation); !ok || p.Event != journal.RotationStart {
				t.Fatalf("rerun did not start rotation: %+v", a)
			}
			for _, seq := range facts {
				if !slices.Contains(a.Cause, seq) {
					t.Fatalf("rerun completion omitted carried fact #%d: %v", seq, a.Cause)
				}
			}
			if !strings.Contains(a.Payload.Message(), "rerun checks passed") {
				t.Fatalf("rerun completion lacks explanation: %s", a.Payload.Message())
			}
			h.decide(a)
			if !refine {
				// Carried reruns do not qualify a guard rotation. Finish its live
				// steps so the next decision exposes stale replay-only citations.
				a = h.next()
				if a.Kind != RunTrial || a.Trial.Rerun || a.Trial.Regime != machine.R1 {
					t.Fatalf("expected live rotation step: %+v", a)
				}
				h.trial(a, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: a.Trial.DurationS})
			}
			replayed := New()
			for _, e := range h.events {
				replayed.Fold(e)
			}
			want, got := h.s.Next(), replayed.Next()
			if want.Kind != Decide {
				t.Fatalf("expected next journal decision: %+v", want)
			}
			if diff := cmp.Diff(want, got); diff != "" {
				t.Fatalf("next decision after completed reruns (-uninterrupted +replayed):\n%s", diff)
			}
			if strings.Contains(got.Payload.Message(), "rerun checks passed") {
				t.Fatalf("completed rerun cited again: %s", got.Payload.Message())
			}
			for _, seq := range facts {
				if slices.Contains(got.Cause, seq) {
					t.Fatalf("historical rerun fact #%d cited again: %v", seq, got.Cause)
				}
			}
		})
	}
}

func TestCarriedRerunBoundaryPreservesPendingChecks(t *testing.T) {
	for _, refine := range []bool{false, true} {
		for _, missingLong := range []bool{false, true} {
			t.Run(fmt.Sprintf("refine=%t/missing-long=%t", refine, missingLong), func(t *testing.T) {
				h := newHarness(t, coreStart{phase: journal.PhaseDone, offset: -10, fail: new(-11)})
				h.add(&journal.ProfileChange{To: []int{-10}})
				failure := carryTrials(h, machine.R1, []int{0}, []int{-10}, 900, 1, journal.OutcomeFailure)[0]
				count, duration := h.s.n-1, h.s.durations.StartS
				if missingLong {
					count, duration = h.s.n, 900
				}
				carryTrials(h, machine.R1, []int{0}, []int{-10}, h.s.durations.StartS, count, journal.OutcomePass)
				h.add(&journal.TunerDecision{Core: 0, Phase: journal.PhaseGuard, Decision: journal.Backoff, FromOffset: -10, ToOffset: -9, FailedMark: new(-10)}, failure)
				h.add(&journal.ProfileChange{From: []int{-10}, To: []int{-9}})
				if refine {
					h.add(&journal.RefineRound{Round: 1, Event: journal.RotationEnd})
				} else {
					h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationStart, Steps: []machine.Regime{machine.R1}})
				}
				replayed := New()
				for _, e := range h.events {
					replayed.Fold(e)
				}
				want, got := h.s.Next(), replayed.Next()
				if got.Kind != RunTrial || !got.Trial.Rerun || got.Trial.DurationS != duration || !slices.Equal(got.Cause, []int{failure}) {
					t.Fatalf("boundary lost pending rerun: %+v", got)
				}
				if diff := cmp.Diff(want, got); diff != "" {
					t.Fatalf("pending rerun (-uninterrupted +replayed):\n%s", diff)
				}
			})
		}
	}
}
