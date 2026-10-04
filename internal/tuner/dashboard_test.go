package tuner

import (
	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"strconv"
	"testing"
)

func TestCoreLimitHolders(t *testing.T) {
	for _, tt := range []struct {
		name    string
		offset  int
		fail    *int
		members []journal.CombinationMember
		want    Limit
	}{
		{name: "room", offset: -20},
		{name: "floor", offset: -50, want: Limit{Floor: true}},
		{name: "failure", offset: -20, fail: new(-21), want: Limit{Failure: true}},
		{name: "combination", offset: -20, members: []journal.CombinationMember{{Core: 0, Offset: -21}, {Core: 1, Offset: -10}}, want: Limit{Combination: 7}},
		{name: "clear combination", offset: -20, members: []journal.CombinationMember{{Core: 0, Offset: -21}, {Core: 1, Offset: -11}}},
	} {
		t.Run(tt.name, func(t *testing.T) {
			h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: tt.offset, fail: tt.fail}, coreStart{phase: journal.PhaseHasRoom, offset: -10})
			if tt.members != nil {
				h.add(&journal.Combination{Combination: 7, Members: tt.members})
			}
			if diff := cmp.Diff(tt.want, h.s.CoreLimit(0)); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestCyclePlanFrozenPartRequirements(t *testing.T) {
	h, _, _ := sevenCorePartialHarness(t)
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{machine.R7, machine.R1}})
	h.decide(h.next())
	plan := h.s.CyclePlan()
	want := []CyclePart{
		{Cores: []int{1, 2, 3, 4, 5, 6, 7}, CCD: 0, RecordOnly: true, Short: 3, ShortS: 120, Long: 1, LongS: 300},
		{Cores: []int{0, 1, 2, 3, 4, 5, 6, 7}, CCD: 0, Full: true, Short: 3, ShortS: 120, Long: 1, LongS: 300},
		{Cores: []int{9, 10, 11, 12, 13, 14, 15}, CCD: 1, RecordOnly: true, Short: 3, ShortS: 120, Long: 1, LongS: 300},
		{Cores: []int{8, 9, 10, 11, 12, 13, 14, 15}, CCD: 1, Full: true, Short: 3, ShortS: 120, Long: 1, LongS: 300},
		{Cores: []int{0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15}, CCD: -1, Full: true, Short: 3, ShortS: 120, Long: 1, LongS: 600},
	}
	if diff := cmp.Diff(want, plan.Steps[0].Parts); diff != "" {
		t.Fatal(diff)
	}
	if len(plan.Steps[1].Parts) != 16 {
		t.Fatal("per-core step lost cores")
	}
	intent := h.start(h.next()).Data.(*journal.TrialIntent)
	h.add(&journal.TrialEnd{Trial: intent.Trial, Outcome: journal.OutcomeFailure, Signal: machine.ComputationError})
	got := h.s.CyclePlan().Steps[0].Parts[0]
	if got.Failed != 1 || got.Done {
		t.Fatalf("partial failure must complete one trial only: %+v", got)
	}
	p := h.start(h.next()).Data.(*journal.TrialIntent)
	if diff := cmp.Diff(TrialRequirement{Failed: 1, Trial: 2, Needed: 3}, h.s.Requirement(p)); diff != "" {
		t.Fatal(diff)
	}
	h.add(&journal.CorePhase{Core: 0, To: journal.PhaseHasRoom, Offset: -30})
	if diff := cmp.Diff(want[0].Cores, h.s.CyclePlan().Steps[0].Parts[0].Cores); diff != "" {
		t.Fatalf("frozen load changed: %s", diff)
	}
}

func TestRerunPlanRetainsFailedLength(t *testing.T) {
	for _, duration := range []int{120, 900} {
		t.Run(strconv.Itoa(duration), func(t *testing.T) {
			h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -10, fail: new(-11)})
			h.add(&journal.ProfileChange{To: []int{-10}})
			failure := carryTrials(h, machine.R1, []int{0}, []int{-10}, duration, 1, journal.OutcomeFailure)[0]
			h.add(&journal.TunerDecision{Core: 0, Phase: journal.PhaseChecking, Decision: journal.Backoff, FromOffset: -10, ToOffset: -9, FailurePoint: new(-10)}, failure)
			h.add(&journal.ProfileChange{From: []int{-10}, To: []int{-9}})
			want := &RerunPlan{Regime: machine.R1, Cores: []int{0}, Short: h.s.n, ShortS: 120}
			if duration != 120 {
				want.Long = 1
				want.LongS = duration
			}
			if diff := cmp.Diff(want, h.s.RerunPlan()); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestRequirementCountsOnlyFreshSearchTrials(t *testing.T) {
	h := newHarness(t, searchAt(-20)...)
	first := h.start(h.next()).Data.(*journal.TrialIntent)
	h.add(&journal.TrialEnd{Trial: first.Trial, Outcome: journal.OutcomePass, DurationS: first.DurationS})
	later := *first
	later.Trial, later.Offset, later.Profile = "later", new(-15), []int{-15}
	if diff := cmp.Diff(TrialRequirement{Trial: 1, Needed: 1}, h.s.Requirement(&later)); diff != "" {
		t.Fatalf("a search step's trial counted an earlier step's pass (-want +got):\n%s", diff)
	}
}

func TestCyclePlanRunsOnlyTheCurrentStepsPart(t *testing.T) {
	h := hasRoomHarness(t, -20, -20)
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{machine.R6, machine.R6, machine.R6, machine.R6}})
	// Occurrences rotate through R6's workloads, so a later step can repeat an earlier step's class.
	for step := range 4 {
		a := h.next()
		for a.Kind == Decide {
			h.decide(a)
			a = h.next()
		}
		p := h.start(a).Data.(*journal.TrialIntent)
		plan := h.s.CyclePlan()
		for i, s := range plan.Steps {
			for _, part := range s.Parts {
				if part.Running != (i == step) {
					t.Fatalf("trial of step %d: step %d part running=%t: %+v", step+1, i+1, part.Running, plan)
				}
			}
		}
		h.add(&journal.TrialEnd{Trial: p.Trial, Outcome: journal.OutcomePass, DurationS: p.DurationS})
	}
}
