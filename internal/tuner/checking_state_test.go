package tuner

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestEarlierCycleCreditsAnUncontradictedShallowerOrEqualProfile(t *testing.T) {
	failure := func(profile []int) journal.Payload {
		return &journal.Failure{Attribution: journal.Unattributed, Signal: machine.Crash, Condition: machine.Together, Regime: machine.R6, Profile: profile}
	}
	for _, tt := range []struct {
		name  string
		after []journal.Payload
		final []int
		want  int
	}{
		{"returns to the passed full-cycle profile", nil, []int{-10, -12}, 1},
		{"shallower than the passed full-cycle profile", nil, []int{-10, -11}, 1},
		{"deeper than the passed full-cycle profile", nil, []int{-11, -12}, 0},
		{"failure at the passed full-cycle profile", []journal.Payload{failure([]int{-10, -12})}, []int{-10, -12}, 0},
		{"failure at a deeper profile", []journal.Payload{failure([]int{-11, -13})}, []int{-10, -12}, 1},
		{"incomplete failure profile", []journal.Payload{failure([]int{-10})}, []int{-10, -12}, 0},
		{"failure at an incomparable profile", []journal.Payload{failure([]int{-11, -11})}, []int{-10, -12}, 1},
		{"failure at a profile as shallow as the passed full-cycle one", []journal.Payload{failure([]int{-9, -12})}, []int{-10, -12}, 0},
		{"reset after the passed full cycle", []journal.Payload{&journal.CommandReset{Core: new(1)}}, []int{-10, -12}, 0},
	} {
		t.Run(tt.name, func(t *testing.T) {
			h := hasRoomHarness(t, -10, -12)
			h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{machine.R1}})
			h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleEnd, Passed: true, Full: true})
			h.add(&journal.ProfileChange{From: []int{-10, -12}, To: []int{-11, -11}})
			for _, p := range tt.after {
				h.add(p)
			}
			h.add(&journal.ProfileChange{From: []int{-11, -11}, To: tt.final})
			if got := h.s.CleanCycles(); got != tt.want {
				t.Fatalf("clean cycles = %d, want %d", got, tt.want)
			}
		})
	}
}

func TestCreditedCycleReplays(t *testing.T) {
	h := hasRoomHarness(t, -10, -12)
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{machine.R1}})
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleEnd, Passed: true, Full: true})
	h.add(&journal.ProfileChange{From: []int{-10, -12}, To: []int{-11, -12}})
	h.add(&journal.ProfileChange{From: []int{-11, -12}, To: []int{-10, -12}})
	state := projected(h)
	if state.Checking.CleanCycles != 1 || state.Checking.LastCleanCycle != 1 {
		t.Fatalf("lost credited cycle: %+v", state.Checking)
	}
	replayed := New()
	for _, event := range h.events {
		replayed.Fold(event)
	}
	var got journal.State
	replayed.Project(&got)
	if diff := cmp.Diff(state.Checking, got.Checking); diff != "" {
		t.Fatalf("credited cycle after replay (-want +got):\n%s", diff)
	}
}

func TestCleanCycleSummaryAfterDeepening(t *testing.T) {
	h := hasRoomHarness(t, -10)
	for _, cycle := range []int{3, 7} {
		h.add(&journal.CheckingCycle{Cycle: cycle, Event: journal.CycleStart, Steps: []machine.Regime{machine.R1}})
		h.add(&journal.CheckingCycle{Cycle: cycle, Event: journal.CycleEnd, Passed: true, Full: true})
	}
	g := projected(h).Checking
	if g.CleanCycles != 2 || g.LastCleanCycle != 7 {
		t.Fatalf("clean cycle summary: %+v", g)
	}
	h.add(&journal.ProfileChange{From: []int{-10}, To: []int{-11}})
	g = projected(h).Checking
	if g.CleanCycles != 0 || g.LastCleanCycle != 0 {
		t.Fatalf("deepening retained shallower cycle credit: %+v", g)
	}
}

func TestEarlierCycleRequiresCoresAtLimitAtItsEnd(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseHasRoom, offset: -10})
	h.decide(h.next())
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{machine.R1}})
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleEnd, Passed: true, Full: true})
	h.add(&journal.CorePhase{Core: 0, To: journal.PhaseAtLimit, Offset: -10, FailurePoint: new(-11)})
	h.add(&journal.ProfileChange{From: []int{-10}, To: []int{-11}})
	h.add(&journal.ProfileChange{From: []int{-11}, To: []int{-10}})
	if got := h.s.CleanCycles(); got != 0 {
		t.Fatalf("cycle that ended before every core was at its limit counted: %d", got)
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
