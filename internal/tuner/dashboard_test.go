package tuner

import (
	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"slices"
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

func TestCyclePlanDynamicChainRequirements(t *testing.T) {
	h := chainHarness(t)
	want := []CyclePart{
		{Cores: []int{0, 1, 2, 3}, CCD: 0, Full: true, Short: 3, ShortS: 120, Long: 1, LongS: 300},
		{Cores: []int{4, 5, 6, 7}, CCD: 1, Full: true, Short: 3, ShortS: 120, Long: 1, LongS: 300},
		{Cores: h.s.ids(), CCD: -1, Full: true, Short: 3, ShortS: 120, Long: 1, LongS: 600},
	}
	if diff := cmp.Diff(want, h.s.CyclePlan().Steps[0].Parts); diff != "" {
		t.Fatalf("invented a partial before its predecessor passed (-want +got):\n%s", diff)
	}
	passChainPart(t, h, want[0].Cores, map[int]float64{0: 1.1, 1: 1.11, 2: 1.2, 3: 1.09})
	plan := h.s.CyclePlan()
	if !plan.Steps[0].Parts[0].Done || plan.Steps[0].Done || plan.Steps[0].ChainsComplete || plan.Current != 0 {
		t.Fatalf("passing a predecessor prematurely completed its chain: %+v", plan)
	}
	h.decide(h.s.cycleNext())
	partial := CyclePart{Cores: []int{0, 1, 3}, CCD: 0, Partial: true, Short: 3, ShortS: 120, Long: 1, LongS: 300}
	if diff := cmp.Diff(partial, h.s.CyclePlan().Steps[0].Parts[1]); diff != "" {
		t.Fatalf("derived partial ignored measured request order (-want +got):\n%s", diff)
	}
	passChainPart(t, h, partial.Cores, map[int]float64{0: 1.12, 1: 1.25, 3: 1.1})
	h.decide(h.s.cycleNext())
	partial.Cores = []int{0, 3}
	plan = h.s.CyclePlan()
	if diff := cmp.Diff(partial, plan.Steps[0].Parts[2]); diff != "" {
		t.Fatalf("second derived partial missing (-want +got):\n%s", diff)
	}
	if got := plan.Steps[0].Parts[1]; !got.Done || got.Passed != 4 {
		t.Fatalf("partial passes did not fulfill ordinary requirements: %+v", got)
	}
	passChainPart(t, h, partial.Cores, nil)
	h.decide(h.s.cycleNext())
	passChainPart(t, h, want[1].Cores, nil)
	h.decide(h.s.cycleNext())
	plan = h.s.CyclePlan()
	if !plan.Steps[0].ChainsComplete || plan.Steps[0].Done || len(plan.Steps[0].Parts) != 5 {
		t.Fatalf("chain endings became loads or bypassed all-core requirements: %+v", plan)
	}
	passChainPart(t, h, want[2].Cores, nil)
	plan = h.s.CyclePlan()
	if !plan.Steps[0].Done || plan.Current != 1 {
		t.Fatalf("completed dynamic chain did not complete the projected step: %+v", plan)
	}
	replay := replayState(h.events)
	if diff := cmp.Diff(plan, replay.CyclePlan()); diff != "" {
		t.Fatalf("chain projection changed across replay (-live +replay):\n%s", diff)
	}
}

func TestCyclePlanPartialFailureNeedsPasses(t *testing.T) {
	h := chainHarness(t)
	passChainPart(t, h, []int{0, 1, 2, 3}, nil)
	h.decide(h.s.cycleNext())
	h.trial(h.s.cycleNext(), passed)
	h.trial(h.s.cycleNext(), failed)
	got := h.s.CyclePlan().Steps[0].Parts[1]
	if got.Passed != 0 || got.Failed != 1 || got.Done {
		t.Fatalf("failure completed a partial trial or retained invalidated passes: %+v", got)
	}
	p := h.start(h.s.cycleNext()).Data.(*journal.TrialIntent)
	if diff := cmp.Diff(TrialRequirement{Failed: 1, Trial: 1, Needed: 3}, h.s.Requirement(p)); diff != "" {
		t.Fatalf("failure advanced the requirement's trial index (-want +got):\n%s", diff)
	}
	if !h.s.CyclePlan().Steps[0].Parts[1].Running {
		t.Fatal("ordinary partial trial is not projected as running")
	}
}

func TestCyclePlanPartialProfileChanges(t *testing.T) {
	for _, started := range []bool{false, true} {
		t.Run(map[bool]string{false: "unstarted", true: "inconclusive"}[started], func(t *testing.T) {
			h := chainHarness(t)
			passChainPart(t, h, []int{0, 1, 2, 3}, nil)
			h.decide(h.s.cycleNext())
			before := h.s.CyclePlan().Steps[0].Parts[1]
			if started {
				h.trial(h.s.cycleNext(), unsure)
			}
			profile := slices.Clone(h.s.Profile())
			profile[1] = -1
			h.add(&journal.ProfileChange{From: h.s.Profile(), To: profile})
			plan := h.s.CyclePlan()
			if started {
				if diff := cmp.Diff(before.Cores, plan.Steps[0].Parts[1].Cores); diff != "" {
					t.Fatalf("inconclusive trial did not freeze the partial: %s", diff)
				}
			} else {
				if len(plan.Steps[0].Parts) != 3 {
					t.Fatalf("stale unstarted partial remained projected: %+v", plan)
				}
				h.decide(h.s.cycleNext())
				if diff := cmp.Diff([]int{0, 2, 3}, h.s.CyclePlan().Steps[0].Parts[1].Cores); diff != "" {
					t.Fatalf("rederived partial was not projected: %s", diff)
				}
			}
			replay := replayState(h.events)
			if diff := cmp.Diff(h.s.CyclePlan(), replay.CyclePlan()); diff != "" {
				t.Fatalf("partial freeze changed across replay: %s", diff)
			}
		})
	}
}

func TestCyclePlanRepeatedPartialRequirements(t *testing.T) {
	h := chainHarnessOn(t, topology(8), config.Default(), machine.R7, machine.R7, machine.R7, machine.R7)
	cores := []int{1, 2, 3}
	workload := machine.Workloads(machine.R7)[0].ID
	for _, step := range []int{1, 4} {
		if step != 1 {
			h.add(&journal.CheckingStep{Cycle: 1, Step: step, Profile: h.s.Profile()})
		}
		h.add(&journal.CheckingChain{Cycle: 1, Step: step, CCD: 0, Workload: workload, Part: "partial 1", Cores: cores, Profile: h.s.Profile()})
	}
	trial := Trial{Regime: machine.R7, Workload: workload, Cores: cores, DurationS: 120, Phase: journal.PhaseChecking, Condition: machine.Together, Cycle: 1, Step: 4}
	for range 2 {
		h.trial(Action{Kind: RunTrial, Trial: trial}, passed)
	}
	p := h.start(Action{Kind: RunTrial, Trial: trial}).Data.(*journal.TrialIntent)
	if diff := cmp.Diff(TrialRequirement{Passed: 2, Trial: 3, Needed: 6}, h.s.Requirement(p)); diff != "" {
		t.Fatalf("explicit step answered an earlier occurrence (-want +got):\n%s", diff)
	}
	plan := h.s.CyclePlan()
	if got := plan.Steps[3].Parts[1]; got.Short != 6 || got.Long != 2 || got.Passed != 2 || got.Done || !got.Running {
		t.Fatalf("repeated partial class did not add requirements: %+v", got)
	}
	if plan.Steps[0].Parts[1].Running {
		t.Fatal("one trial marked an earlier partial occurrence as running")
	}
}

func TestCyclePlanPartialEvidenceWindows(t *testing.T) {
	for _, carried := range []bool{false, true} {
		t.Run(map[bool]string{false: "legacy live partial", true: "carried partial"}[carried], func(t *testing.T) {
			h := chainHarness(t)
			passChainPart(t, h, []int{0, 1, 2, 3}, nil)
			h.decide(h.s.cycleNext())
			cores := []int{1, 2, 3}
			workload := machine.Workloads(machine.R7)[0].ID
			for range 3 {
				if carried {
					h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "old", Seq: len(h.events) + 1, Trial: "partial"}, Class: journal.TrialClass{Regime: machine.R7, Workload: workload, Cores: cores, DurationS: 120}, Profile: h.s.Profile(), Condition: machine.Together, Phase: journal.PhaseChecking, RecordOnly: true, Outcome: journal.OutcomePass})
				} else {
					h.add(&journal.TrialIntent{Trial: "legacy partial", Regime: machine.R7, Workload: workload, Cores: cores, DurationS: 120, Profile: h.s.Profile(), Condition: machine.Together, Phase: journal.PhaseChecking, RecordOnly: true})
					h.add(&journal.TrialEnd{Trial: "legacy partial", Outcome: journal.OutcomePass, DurationS: 120})
				}
			}
			want := 3
			if carried {
				want = 0
			}
			part := h.s.CyclePlan().Steps[0].Parts[1]
			if part.Passed != want || part.Done {
				t.Fatalf("projected partial did not use ordinary live cycle evidence: %+v, want %d passes", part, want)
			}
			p := &journal.TrialIntent{Regime: machine.R7, Workload: workload, Cores: cores, DurationS: 300, Profile: h.s.Profile(), Condition: machine.Together, Phase: journal.PhaseChecking, Cycle: 1, Step: 1}
			if diff := cmp.Diff(TrialRequirement{Trial: 1, Needed: 1}, h.s.Requirement(p)); diff != "" {
				t.Fatalf("short partial evidence leaked into long class: %s", diff)
			}
		})
	}
}

func TestCyclePlanOneCCDMergesCoincidentDurations(t *testing.T) {
	cfg := config.Default()
	cfg.Durations.ShortTrialS = cfg.Durations.CheckingAllCoreS
	h := chainHarnessOn(t, onOneCCD(topology(8)), cfg, machine.R7)
	plan := h.s.CyclePlan()
	if got := plan.Steps[0].Parts; len(got) != 1 || !got[0].Full || got[0].Short != 4 || got[0].Long != 0 {
		t.Fatalf("one CCD duplicated its full part or split one trial class: %+v", got)
	}
	passChainPart(t, h, h.s.ids(), nil)
	h.decide(h.s.cycleNext())
	part := h.s.CyclePlan().Steps[0].Parts[1]
	if !part.Partial || part.Full || part.Short != 4 || part.Long != 0 || part.ShortS != h.s.durations.ShortTrialS {
		t.Fatalf("partial did not merge coincident short/long requirements: %+v", part)
	}
	p := h.start(h.s.cycleNext()).Data.(*journal.TrialIntent)
	if diff := cmp.Diff(TrialRequirement{Trial: 1, Needed: 4}, h.s.Requirement(p)); diff != "" {
		t.Fatalf("merged partial class has wrong trial requirement: %s", diff)
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

func TestRequirementCountsAloneDeepeningChecksAgainstTheirRound(t *testing.T) {
	h := hasRoomHarness(t, -20, -20)
	p := h.s.Profile()
	p[1]--
	h.add(&journal.DeepeningRound{Round: 2, Event: journal.CycleStart, Profile: p, Target: p, Cores: []int{1}, Trials: 3, TrialS: 120})
	for passed := range 2 {
		a := h.s.roundCheck()
		if a.Kind != RunTrial || a.Trial.Condition != machine.Alone || a.Trial.Round != 2 {
			t.Fatalf("deepening did not check core 01 alone: %+v", a)
		}
		intent := h.start(a).Data.(*journal.TrialIntent)
		want := TrialRequirement{Passed: passed, Trial: passed + 1, Needed: 3}
		if diff := cmp.Diff(want, h.s.Requirement(intent)); diff != "" {
			t.Fatalf("an alone deepening check counted as a search step (-want +got):\n%s", diff)
		}
		h.add(&journal.TrialEnd{Trial: intent.Trial, Outcome: journal.OutcomePass, DurationS: intent.DurationS})
	}
}

func TestCyclePlanRunsNoPartDuringARerun(t *testing.T) {
	h := hasRoomHarness(t, -20, -20, -20, -20)
	h.add(&journal.HostRanking{Ranking: h.s.ids()})
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{machine.R7}})
	a := h.next()
	for a.Kind == Decide {
		h.decide(a)
		a = h.next()
	}
	p := h.start(a).Data.(*journal.TrialIntent)
	h.add(&journal.TrialEnd{Trial: p.Trial, Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, DurationS: 10})
	a = h.next()
	for a.Kind == Decide || a.Trial.Hunt > 0 {
		if a.Kind == RunTrial {
			// The located hunt's locate fails too, so the failure stays with the loaded CCD.
			h.trial(a, failed)
		} else {
			h.decide(a)
		}
		a = h.next()
	}
	if a.Kind != RunTrial || !a.Trial.Rerun || !slices.Equal(a.Trial.Cores, p.Cores) || a.Trial.DurationS != p.DurationS {
		t.Fatalf("the R7 failure did not rerun its part's load at the part's length: %+v", a)
	}
	h.start(a)
	plan := h.s.CyclePlan()
	if !plan.Paused {
		t.Fatalf("the cycle is not paused while its failure reruns: %+v", plan)
	}
	// The rerun loads the failed part's class, so only the rerun exclusion keeps that part from running.
	for _, step := range plan.Steps {
		for _, part := range step.Parts {
			if part.Running {
				t.Fatalf("the rerun ran as a part of the paused cycle: %+v", plan)
			}
		}
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

func TestHuntCutCountsSplitsAcrossTheHunt(t *testing.T) {
	group := func(stage string, set []int, g, index, duration int) groupRecord {
		return groupRecord{payload: &journal.HuntGroup{Stage: stage, Set: set, Granularity: g, Index: index, DurationS: duration}}
	}
	all := []int{0, 1, 2, 3}
	h := &hunt{start: &journal.HuntStart{}, groups: []groupRecord{
		group("part", all, 2, 0, 60),
		group("part", all, 2, 1, 60),
		group("complement", all, 2, 0, 60),
		group("part", []int{0, 1}, 2, 0, 60),
	}}
	for _, tt := range []struct {
		name string
		plan groupPlan
		want int
	}{
		{"first split, its complements included", groupPlan{stage: "complement", set: all, g: 2, duration: 60}, 1},
		{"failed part cut finer", groupPlan{stage: "part", set: []int{0, 1}, g: 2, duration: 60}, 2},
		{"next split not yet run", groupPlan{stage: "part", set: []int{0}, g: 2, duration: 60}, 3},
		{"the same cores at another trial length", groupPlan{stage: "part", set: all, g: 2, duration: 120}, 3},
		{"not a split", groupPlan{stage: "full", set: all, duration: 60}, 0},
	} {
		if got := huntCut(h, tt.plan); got != tt.want {
			t.Errorf("%s: cut %d, want %d", tt.name, got, tt.want)
		}
	}
}
