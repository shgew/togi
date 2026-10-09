package tuner

import (
	"fmt"
	"math"
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestCarriedRerunCitationReplay(t *testing.T) {
	for _, deepening := range []bool{false, true} {
		for _, reload := range []bool{false, true} {
			t.Run(fmt.Sprintf("deepening=%t/reload=%t", deepening, reload), func(t *testing.T) {
				h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -10, fail: new(-11)})
				cfg := config.Default()
				cfg.Checking.Cycle = []machine.Regime{machine.R1}
				h.add(&journal.ConfigLoaded{Config: snapshotConfig(cfg)})
				h.add(&journal.ProfileChange{To: []int{-10}})
				failure := carryTrials(h, machine.R1, []int{0}, []int{-10}, 900, 1, journal.OutcomeFailure)[0]
				count := h.s.n
				if reload {
					count--
				}
				facts := carryTrials(h, machine.R1, []int{0}, []int{-10}, h.s.durations.ShortTrialS, count, journal.OutcomePass)
				facts = append(facts, carryTrials(h, machine.R1, []int{0}, []int{-10}, 900, 1, journal.OutcomePass)...)
				if deepening {
					h.add(&journal.DeepeningRound{Round: 1, Event: journal.CycleStart, Profile: []int{-9}, Target: []int{-9}, Cores: []int{0}, Trials: h.s.n, TrialS: h.s.durations.ShortTrialS})
				}
				h.add(&journal.TunerDecision{Core: 0, Phase: journal.PhaseChecking, Decision: journal.Backoff, FromOffset: -10, ToOffset: -9, FailurePoint: new(-10)}, failure)
				h.add(&journal.ProfileChange{From: []int{-10}, To: []int{-9}})
				if reload {
					// A supported configuration reload lowers the evidence threshold
					// so the existing carried observations complete the rerun.
					cfg.Evidence.Miss = math.Pow(1-cfg.Evidence.Rate, float64(h.s.n-1)) * 1.01
					h.add(&journal.ConfigLoaded{Config: snapshotConfig(cfg)})
				}
				a := h.next()
				if deepening {
					if p, ok := a.Payload.(*journal.DeepeningRound); !ok || p.Event != journal.CycleEnd || !p.Passed {
						t.Fatalf("rerun did not finish deepening: %+v", a)
					}
				} else if p, ok := a.Payload.(*journal.CheckingCycle); !ok || p.Event != journal.CycleStart {
					t.Fatalf("rerun did not start cycle: %+v", a)
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
				if !deepening {
					// Carried reruns do not supply full-cycle coverage. Finish its live
					// steps so the next decision exposes stale replay-only citations.
					a = h.next()
					if a.Kind != RunTrial || a.Trial.Rerun || a.Trial.Regime != machine.R1 {
						t.Fatalf("expected live cycle step: %+v", a)
					}
					h.trial(a, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: a.Trial.DurationS})
				}
				replayed := replayState(h.events)
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
}

func TestCarriedRerunBoundaryPreservesPendingChecks(t *testing.T) {
	for _, deepening := range []bool{false, true} {
		for _, missingLong := range []bool{false, true} {
			t.Run(fmt.Sprintf("deepening=%t/missing-long=%t", deepening, missingLong), func(t *testing.T) {
				h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -10, fail: new(-11)})
				h.add(&journal.ProfileChange{To: []int{-10}})
				failure := carryTrials(h, machine.R1, []int{0}, []int{-10}, 900, 1, journal.OutcomeFailure)[0]
				count, duration := h.s.n-1, h.s.durations.ShortTrialS
				if missingLong {
					count, duration = h.s.n, 900
				}
				carryTrials(h, machine.R1, []int{0}, []int{-10}, h.s.durations.ShortTrialS, count, journal.OutcomePass)
				h.add(&journal.TunerDecision{Core: 0, Phase: journal.PhaseChecking, Decision: journal.Backoff, FromOffset: -10, ToOffset: -9, FailurePoint: new(-10)}, failure)
				h.add(&journal.ProfileChange{From: []int{-10}, To: []int{-9}})
				if deepening {
					h.add(&journal.DeepeningRound{Round: 1, Event: journal.CycleEnd})
				} else {
					h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{machine.R1}})
				}
				replayed := replayState(h.events)
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
