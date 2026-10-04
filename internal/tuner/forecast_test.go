package tuner

import (
	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"testing"
)

func TestRecordOnlyForecastDoesNotBackoff(t *testing.T) {
	h, _, _ := sevenCorePartialHarness(t)
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{machine.R7}})
	h.decide(h.next())
	h.start(h.next())
	f := Forecast(h.events)
	for _, b := range f.Branches {
		if b.Premise == IfNamed || b.Premise == IfUnnamed || b.Premise == IfPass {
			if len(b.Decisions) != 0 || b.Next == nil || !b.Next.RecordOnly {
				t.Fatalf("record-only branch changed tuning: %+v", b)
			}
		}
	}
}

func TestForecastAllPassCountsPartialFailures(t *testing.T) {
	h, _, _ := sevenCorePartialHarness(t)
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{machine.R7}})
	h.decide(h.next())
	h.trial(h.next(), failed)
	h.start(h.next())
	f := Forecast(h.events)
	var pass, all *ForecastBranch
	for i := range f.Branches {
		b := &f.Branches[i]
		if b.Premise == IfPass {
			pass = b
		}
		if b.Premise == IfAllPass {
			all = b
		}
	}
	if pass == nil || all == nil || all.Passes != 2 || pass.Next == nil || all.Next == nil {
		t.Fatalf("missing remaining-trial branch: %+v", f)
	}
	if pass.Next.DurationS != 120 || all.Next.DurationS != 300 || !all.Next.RecordOnly {
		t.Fatalf("partial requirement did not complete: pass=%+v all=%+v", pass, all)
	}
}

func TestForecastNeedsRankingWithoutRecordedRead(t *testing.T) {
	h := hasRoomHarness(t, -20, -20)
	h.start(Action{Kind: RunTrial, Trial: Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, DurationS: 120, Phase: journal.PhaseChecking, Condition: machine.Together}})
	f := Forecast(h.events)
	for _, b := range f.Branches {
		if b.Premise == IfUnnamed {
			if !b.NeedsRanking || b.Next != nil {
				t.Fatalf("invented ranking: %+v", b)
			}
			return
		}
	}
	t.Fatal("missing unnamed branch")
}

func TestForecastSingleLoadedTogetherMayBeUnattributed(t *testing.T) {
	h := hasRoomHarness(t, -20, -20)
	h.start(Action{Kind: RunTrial, Trial: Trial{Core: 0, Offset: -20, Regime: machine.R1, Workload: machine.Workloads(machine.R1)[0].ID, DurationS: 120, Phase: journal.PhaseChecking, Condition: machine.Together}})
	f := Forecast(h.events)
	for _, b := range f.Branches {
		if b.Premise == IfUnnamed {
			failure, ok := b.Decisions[0].(*journal.Failure)
			if !ok || failure.Attribution != journal.Unattributed {
				t.Fatalf("wrong attribution: %+v", b)
			}
			return
		}
	}
	t.Fatal("suppressed a possible unnamed failure")
}

func TestHuntPlanRunningMemberIsNotDone(t *testing.T) {
	h := huntHarness(t, 4, 120)
	for range 300 {
		a := h.next()
		if group, ok := a.Payload.(*journal.HuntGroup); ok {
			h.decide(a)
			if group.Probe != nil {
				h.start(h.next())
				plan := h.s.HuntPlan()
				for _, p := range plan.Probes {
					if p.Member == group.Probe.Core {
						want := MemberProbe{Member: group.Probe.Core, Offset: group.Probe.Offset, FailedAt: []int{-30}, Running: true}
						if diff := cmp.Diff(want, p); diff != "" {
							t.Fatal(diff)
						}
						return
					}
				}
				t.Fatal("running member missing")
			}
			continue
		}
		if a.Kind == RunTrial {
			runGroup(h, a, a.Trial.Profile[0] <= -27 && a.Trial.Profile[1] <= -27)
			continue
		}
		if a.Kind == Decide {
			t.Fatalf("hunt stopped before probes: %+v", a)
		}
	}
	t.Fatal("hunt never probed a member")
}

func TestForecastInconclusiveRetainsTrial(t *testing.T) {
	for _, recordOnly := range []bool{false, true} {
		h := newHarness(t, searchAt(-20)...)
		if recordOnly {
			h, _, _ = sevenCorePartialHarness(t)
			h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{machine.R7}})
			h.decide(h.next())
		}
		p := h.start(h.next()).Data.(*journal.TrialIntent)
		want := trialFromIntent(p)
		want.Retry = true
		found := false
		for _, b := range Forecast(h.events).Branches {
			if b.Premise == IfInconclusive {
				found = true
				if len(b.Decisions) != 0 {
					t.Fatalf("inconclusive changed tuning: %+v", b)
				}
				if diff := cmp.Diff(&want, b.Next); diff != "" {
					t.Fatal(diff)
				}
			}
		}
		if !found {
			t.Fatal("missing inconclusive branch")
		}
	}
}

func TestForecastReusesRecordedRanking(t *testing.T) {
	h := hasRoomHarness(t, -20, -20)
	h.add(&journal.HostRanking{Ranking: []int{1, 0}})
	h.start(Action{Kind: RunTrial, Trial: Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, DurationS: 120, Phase: journal.PhaseChecking, Condition: machine.Together}})
	for _, b := range Forecast(h.events).Branches {
		if b.Premise == IfUnnamed {
			if b.NeedsRanking || b.Next == nil || b.Next.Condition != machine.Parked {
				t.Fatalf("recorded ranking was not usable: %+v", b)
			}
			for _, d := range b.Decisions {
				if p, ok := d.(*journal.HuntStart); ok {
					if diff := cmp.Diff([]int{1, 0}, p.Ranking); diff != "" {
						t.Fatal(diff)
					}
					return
				}
			}
			t.Fatal("hunt not recorded")
		}
	}
	t.Fatal("missing unnamed branch")
}

func TestForecastWaitsForInitialCorePhases(t *testing.T) {
	events := []journal.Event{
		{Seq: 1, Data: &journal.SessionStart{Cores: []machine.CoreInfo{{Core: 0}, {Core: 1}}}},
		{Seq: 2, Data: &journal.TrialIntent{Trial: "preparing", Core: new(0), Regime: machine.R1, Condition: machine.Alone}},
		{Seq: 3, Data: &journal.CorePhase{Core: 0, To: journal.PhaseSearch}},
	}
	for n := 1; n <= len(events); n++ {
		if diff := cmp.Diff(ForecastPlan{}, Forecast(events[:n])); diff != "" {
			t.Fatal(diff)
		}
		s := New()
		for _, e := range events[:n] {
			s.Fold(e)
		}
		if len(s.DeepeningPlan().Room) != 0 {
			t.Fatal("uninitialized core was given room")
		}
	}
}

func TestForecastDetectsRecurringHuntPlan(t *testing.T) {
	h := newHarness(t,
		coreStart{phase: journal.PhaseHasRoom, offset: -20},
		coreStart{phase: journal.PhaseAtLimit, offset: -30},
		coreStart{phase: journal.PhaseHasRoom, offset: -10})
	h.add(&journal.ProfileChange{To: []int{-20, -30, -10}})
	h.add(&journal.HuntStart{Hunt: 1, Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0}, Parked: []int{-10, -30, -5}, Failing: []int{-20, -30, -10}, Candidates: []int{0, 2}, Trials: 5, TrialS: 120, DurationS: 120})
	h.add(&journal.HuntGroup{Hunt: 1, Group: 1, Stage: "probe", Cores: []int{2}, Probe: &journal.CombinationMember{Core: 0, Offset: -15}, Held: []journal.CombinationMember{{Core: 2, Offset: -5}}, Profile: []int{-15, -30, -5}, DurationS: 120})
	h.start(Action{Kind: RunTrial, Trial: Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0}, Profile: []int{-15, -30, -5}, DurationS: 120, Hunt: 1, Group: 1, Phase: journal.PhaseHunt, Condition: machine.Parked}})
	found := false
	for _, b := range Forecast(h.events).Branches {
		if b.NeedsHistory {
			found = true
			if b.Next != nil || b.NeedsRanking {
				t.Fatalf("recurring plan invented an outcome: %+v", b)
			}
		}
	}
	if !found {
		t.Fatal("sparse probe history did not report recurring plans")
	}
}

func TestHuntProbeWithoutRecordedCheckingProfile(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseHasRoom, offset: -20}, coreStart{phase: journal.PhaseHasRoom, offset: -10})
	h.add(&journal.HuntStart{Hunt: 1, Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, Failing: []int{-20, -10}, Parked: []int{-10, -5}, Candidates: []int{0, 1}, Trials: 5, TrialS: 120, DurationS: 120})
	h.add(&journal.HuntGroup{Hunt: 1, Group: 1, Stage: "probe", Set: []int{0, 1}, Granularity: 2, Probe: &journal.CombinationMember{Core: 0, Offset: -15}, Held: []journal.CombinationMember{{Core: 1, Offset: -8}}, Profile: []int{-15, -8}, DurationS: 120})
	h.start(Action{Kind: RunTrial, Trial: Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, Profile: []int{-15, -8}, DurationS: 120, Hunt: 1, Group: 1, Phase: journal.PhaseHunt, Condition: machine.Parked}})
	plan := h.s.HuntPlan()
	if len(plan.Probes) != 2 || !plan.Probes[0].Running || plan.Probes[0].Offset != -15 || plan.Groups[0].Probe == nil || len(plan.Groups[0].Held) != 1 {
		t.Fatalf("lost recorded probe roles: %+v", plan)
	}
	for _, b := range Forecast(h.events).Branches {
		if b.Premise == IfAllPass {
			if !b.NeedsHistory || b.Next != nil {
				t.Fatalf("missing profile invented future member plan: %+v", b)
			}
			return
		}
	}
	t.Fatal("missing all-pass branch")
}
