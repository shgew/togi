package tuner

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestConcludedPhasesReplay(t *testing.T) {
	h := hasRoomHarness(t, -10, -12)
	for n := 1; n <= 2; n++ {
		h.add(&journal.CheckingCycle{Cycle: n, Event: journal.CycleStart, Steps: []machine.Regime{machine.R1}})
		h.add(&journal.CheckingCycle{Cycle: n, Event: journal.CycleEnd, Passed: true, Full: true})
	}
	state := projected(h)
	if state.Checking.CleanCycles != 1 || state.Checking.LastCleanCycle != 2 {
		t.Fatalf("confirmation cycle did not conclude phase 2: %+v", state.Checking)
	}
	replayed := replayState(h.events)
	var got journal.State
	replayed.Project(&got)
	if diff := cmp.Diff(state.Checking, got.Checking); diff != "" {
		t.Fatalf("concluded phase 2 after replay (-want +got):\n%s", diff)
	}
	if h.s.phases.phase1End != replayed.phases.phase1End || h.s.phases.confirmStart != replayed.phases.confirmStart || h.s.phases.concluded != replayed.phases.concluded {
		t.Fatalf("phases after replay: live %+v, replayed %+v", h.s.phases, replayed.phases)
	}
}

func TestUnpassedCycleStepsDoneReplays(t *testing.T) {
	h := hasRoomHarness(t, -10, -12)
	for projected(h).Checking.StepsDone < 2 {
		a := h.next()
		if a.Kind == RunTrial {
			h.trial(a, passed)
		} else {
			h.decide(a)
		}
	}
	h.add(&journal.CheckingCycle{Cycle: h.s.checking.cycle, Event: journal.CycleEnd, Reason: "core 00 was reset"})
	live := projected(h)
	if diff := cmp.Diff(2, live.Checking.StepsDone); diff != "" {
		t.Fatalf("steps done (-want +got):\n%s", diff)
	}
	assertProjectionReplay(h)
}

func TestCleanCyclesCountFromPhase2Conclusion(t *testing.T) {
	h := hasRoomHarness(t, -10, -12)
	cycle := func(n int) {
		h.add(&journal.CheckingCycle{Cycle: n, Event: journal.CycleStart, Steps: []machine.Regime{machine.R1}})
		h.add(&journal.CheckingCycle{Cycle: n, Event: journal.CycleEnd, Passed: true, Full: true})
	}
	cycle(1)
	if g := projected(h).Checking; g.CleanCycles != 0 || g.LastCleanCycle != 0 {
		t.Fatalf("phase 1's cycle counted: %+v", g)
	}
	cycle(2)
	if g := projected(h).Checking; g.CleanCycles != 1 || g.LastCleanCycle != 2 {
		t.Fatalf("confirmation cycle did not count once: %+v", g)
	}
	h.add(&journal.ProfileChange{From: []int{-10, -12}, To: []int{-9, -12}})
	cycle(3)
	if g := projected(h).Checking; g.CleanCycles != 2 || g.LastCleanCycle != 3 {
		t.Fatalf("indefinite checking cycle after a step back: %+v", g)
	}
}

func TestResetReturnsToPhase1(t *testing.T) {
	h := hasRoomHarness(t, -10, -12)
	for n := 1; n <= 2; n++ {
		h.add(&journal.CheckingCycle{Cycle: n, Event: journal.CycleStart, Steps: []machine.Regime{machine.R1}})
		h.add(&journal.CheckingCycle{Cycle: n, Event: journal.CycleEnd, Passed: true, Full: true})
	}
	if h.s.CleanCycles() != 1 || h.s.phases.concluded == 0 {
		t.Fatalf("phase 2 not concluded: %d clean cycles", h.s.CleanCycles())
	}
	h.add(&journal.CommandReset{Core: new(1)})
	h.add(&journal.CorePhase{Core: 1, From: journal.PhaseAtLimit, To: journal.PhaseSearch, Offset: -12})
	if h.s.CleanCycles() != 0 || h.s.phases.phase1End != 0 || h.s.phases.concluded != 0 {
		t.Fatalf("reset kept phase state: %+v, %d clean cycles", h.s.phases, h.s.CleanCycles())
	}
}

func TestTemperaturePeakSinceLastProfileChange(t *testing.T) {
	h := hasRoomHarness(t, -10, -12)
	tr := Trial{Regime: machine.R6, Cores: []int{0, 1}, Condition: machine.Together, Phase: journal.PhaseChecking, DurationS: 120}
	_, peak := h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: 120, TctlMaxC: new(92)})
	before := projected(h).Checking
	if before.TctlMaxC == nil || *before.TctlMaxC != 92 || before.TctlMaxSeq != peak.Seq {
		t.Fatalf("missing together peak: %+v", before)
	}
	h.add(&journal.ProfileChange{From: []int{-10, -12}, To: []int{-9, -12}})
	after := projected(h).Checking
	if after.TctlMaxC != nil || after.TctlMaxSeq != 0 {
		t.Fatalf("profile change retained old peak: %+v", after)
	}
	_, next := h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: 120, TctlMaxC: new(81)})
	h.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Condition: machine.Together, Profile: []int{-9, -12}})
	after = projected(h).Checking
	if after.TctlMaxC == nil || *after.TctlMaxC != 81 || after.TctlMaxSeq != next.Seq {
		t.Fatalf("failure without profile change cleared together peak: %+v", after)
	}
}

func TestTemperatureCountsOnlyTogetherPasses(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -10}, coreStart{phase: journal.PhaseAtLimit, offset: -12})
	tr := Trial{Regime: machine.R6, Cores: []int{0, 1}, Condition: machine.Together, Phase: journal.PhaseChecking, DurationS: 120, Profile: []int{-10, -12}}
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: 120, TctlMaxC: new(99)})
	h.add(&journal.ProfileChange{To: []int{-10, -12}})
	for _, tc := range []struct {
		condition machine.Condition
		outcome   journal.Outcome
	}{{machine.Parked, journal.OutcomePass}, {machine.Alone, journal.OutcomePass}, {machine.Together, journal.OutcomeFailure}, {machine.Together, journal.OutcomeInconclusive}} {
		tr.Condition = tc.condition
		h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: tc.outcome, DurationS: 120, TctlMaxC: new(98)})
	}
	tr.Condition = machine.Together
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: 120})
	g := projected(h).Checking
	if g.TctlMaxC != nil || g.TctlMaxSeq != 0 {
		t.Fatalf("uncounted or absent temperature supplied peak: %+v", g)
	}
	var first int
	for _, r := range []machine.Regime{machine.R1, machine.R2} {
		tr.Regime = r
		_, end := h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: 120, TctlMaxC: new(84)})
		if first == 0 {
			first = end.Seq
		}
	}
	g = projected(h).Checking
	if g.TctlMaxC == nil || *g.TctlMaxC != 84 || g.TctlMaxSeq != first {
		t.Fatalf("equal peak must cite earliest counted pass: %+v", g)
	}
}
