package tuner

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestResetClearsCombinationAndReevaluatesOtherCores(t *testing.T) {
	h := newHarness(t,
		coreStart{phase: journal.PhaseAtLimit, offset: -30, fail: new(-31)},
		coreStart{phase: journal.PhaseAtLimit, offset: -30},
	)
	h.add(&journal.SessionBaseline{Offsets: []int{-10, -10}})
	h.add(&journal.Combination{Combination: 1, Hunt: 1, Members: []journal.CombinationMember{{Core: 0, Offset: -30}, {Core: 1, Offset: -31}}})
	h.add(&journal.CommandReset{Core: new(0)})
	a := h.next()
	reset, ok := a.Payload.(*journal.CorePhase)
	if !ok || reset.To != journal.PhaseSearch || reset.Offset != -10 || cmp.Diff([]int{1}, reset.ClearedCombination) != "" {
		t.Fatalf("reset action %+v", a)
	}
	h.decide(a)
	a = h.next()
	other, ok := a.Payload.(*journal.CorePhase)
	if !ok || other.Core != 1 || other.From != journal.PhaseAtLimit || other.To != journal.PhaseHasRoom {
		t.Fatalf("other core remains incorrectly at its limit after combination reset: %+v", a)
	}
}

func TestResetAfterHuntEndDropsCommitment(t *testing.T) {
	h := hasRoomHarness(t, -29, -30)
	h.add(&journal.HuntStart{Hunt: 1, Failure: 10, Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, DurationS: 120, Failing: []int{-29, -30}, Parked: []int{0, 0}, Candidates: []int{0, 1}, Trials: 5, TrialS: 120})
	h.add(&journal.HuntEnd{Hunt: 1, Result: "culprit", Cores: []int{0}, Groups: 2})
	h.add(&journal.CommandReset{Core: new(0)})
	a := h.next()
	p, ok := a.Payload.(*journal.CorePhase)
	if !ok || p.Core != 0 || p.To != journal.PhaseSearch {
		t.Fatalf("reset action %+v", a)
	}
	h.decide(a)
	if h.s.hunt != nil {
		t.Fatal("ended hunt survived the core reset")
	}
	a = h.next()
	if d, ok := a.Payload.(*journal.TunerDecision); ok && d.Phase == journal.PhaseHunt {
		t.Fatalf("reset core received hunt commitment: %+v", a)
	}
	if _, ok := a.Payload.(*journal.Combination); ok {
		t.Fatalf("reset core received combination: %+v", a)
	}
}

func TestResetClosesRoundAndCycleBeforeSearching(t *testing.T) {
	for _, deepening := range []bool{false, true} {
		h := cleanCycleHarness(t, []int{-10}, nil)
		h.add(&journal.SessionBaseline{Offsets: []int{-7}})
		if deepening {
			h.add(&journal.DeepeningRound{Round: 2, Event: journal.CycleStart, Profile: []int{-30}, Target: []int{-50}, Cores: []int{0}, Trials: h.s.n, TrialS: h.s.durations.ShortTrialS})
		} else {
			h.add(&journal.CheckingCycle{Cycle: 4, Event: journal.CycleStart, Steps: h.s.steps})
		}
		reset := h.add(&journal.CommandReset{Core: new(0)})
		a := h.next()
		if deepening {
			r, ok := a.Payload.(*journal.DeepeningRound)
			if !ok || r.Round != 2 || r.Event != journal.CycleEnd || r.Passed {
				t.Fatalf("reset must cancel deepening first: %+v", a)
			}
		} else {
			g, ok := a.Payload.(*journal.CheckingCycle)
			if !ok || g.Cycle != 4 || g.Event != journal.CycleEnd || g.Passed || g.Full {
				t.Fatalf("reset must close cycle without passing: %+v", a)
			}
		}
		if diff := cmp.Diff([]int{reset.Seq}, a.Cause); diff != "" {
			t.Fatalf("reset cancellation cause (-want +got):\n%s", diff)
		}
		h.decide(a)
		a = h.next()
		p, ok := a.Payload.(*journal.CorePhase)
		if !ok || p.To != journal.PhaseSearch || p.Offset != -7 || p.Pass != nil || p.FailurePoint != nil {
			t.Fatalf("reset must restart baseline search: %+v", a)
		}
		h.decide(a)
		assertProjectionReplay(h)
	}
}
