package tuner

import (
	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"slices"
	"testing"
)

func forecastPartialHarness(t *testing.T) *harness {
	t.Helper()
	h := chainHarness(t)
	h.add(&journal.HostRanking{Ranking: h.s.ids()})
	passChainPart(t, h, []int{0, 1, 2, 3}, nil)
	h.decide(h.s.cycleNext())
	return h
}

func TestForecastFormerRecordOnlyPartialRequiresBackoff(t *testing.T) {
	h := forecastPartialHarness(t)
	a := h.s.cycleNext()
	intent, _, err := a.Trial.Complete(0, h.s.Profile())
	if err != nil {
		t.Fatal(err)
	}
	intent.Trial, intent.RecordOnly = "legacy partial", true
	h.add(intent)
	f := Forecast(h.events)
	named, unnamed, all := false, false, false
	for _, b := range f.Branches {
		switch b.Premise {
		case IfNamed:
			named = true
			if !slices.ContainsFunc(b.Decisions, func(d journal.Payload) bool {
				p, ok := d.(*journal.TunerDecision)
				return ok && p.Decision == journal.Backoff
			}) || b.Next == nil || !b.Next.Rerun {
				t.Fatalf("former record-only partial did not require an ordinary backoff/rerun: %+v", b)
			}
		case IfUnnamed:
			unnamed = true
			if !slices.ContainsFunc(b.Decisions, func(d journal.Payload) bool { _, ok := d.(*journal.HuntStart); return ok }) || b.Next == nil || b.Next.Condition != machine.Parked {
				t.Fatalf("an unnamed partial failure with idle cores off CO 0 did not start its located hunt: %+v", b)
			}
		case IfPass:
			if len(b.Decisions) != 0 || b.Next == nil {
				t.Fatalf("partial pass did not continue ordinary checking: %+v", b)
			}
		case IfAllPass:
			all = true
			if b.Passes != 3 || b.Next == nil || b.Next.DurationS != 300 {
				t.Fatalf("legacy record-only marker split the ordinary pass requirement: %+v", b)
			}
		case IfInconclusive:
		}
	}
	if !named || !unnamed || !all {
		t.Fatalf("missing partial premises: %+v", f)
	}
}

func TestForecastAllPassCountsPartialPasses(t *testing.T) {
	h := forecastPartialHarness(t)
	h.trial(h.s.cycleNext(), passed)
	h.start(h.s.cycleNext())
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
		t.Fatalf("missing remaining-pass branch: %+v", f)
	}
	if pass.Next.DurationS != 120 || all.Next.DurationS != 300 {
		t.Fatalf("partial requirement did not complete with passes: pass=%+v all=%+v", pass, all)
	}
}

func TestForecastNeedsRankingWithoutRecordedRead(t *testing.T) {
	h := hasRoomHarness(t, -20, -20, -20, -20)
	h.start(Action{Kind: RunTrial, Trial: Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, DurationS: 120, Phase: journal.PhaseChecking, Condition: machine.Together}})
	for _, b := range Forecast(h.events).Branches {
		if b.Premise == IfUnnamed {
			if !b.NeedsRanking || b.Next != nil {
				t.Fatalf("invented ranking for tied R7 requesters: %+v", b)
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
	for _, partial := range []bool{false, true} {
		h := newHarness(t, searchAt(-20)...)
		if partial {
			h = forecastPartialHarness(t)
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
	h := hasRoomHarness(t, -20, -20, 0, 0)
	h.add(&journal.HostRanking{Ranking: []int{1, 0, 2, 3}})
	h.start(Action{Kind: RunTrial, Trial: Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, DurationS: 120, Phase: journal.PhaseChecking, Condition: machine.Together}})
	for _, b := range Forecast(h.events).Branches {
		if b.Premise == IfUnnamed {
			if b.NeedsRanking || b.Next == nil || !b.Next.Rerun || b.Next.Condition != machine.Together {
				t.Fatalf("recorded ranking was not usable: %+v", b)
			}
			for _, d := range b.Decisions {
				if p, ok := d.(*journal.TunerDecision); ok && p.Decision == journal.Backoff {
					if p.Core != 0 || p.ToOffset != -19 {
						t.Fatalf("wrong tied-top backoff: %+v", p)
					}
					return
				}
			}
			t.Fatal("voltage-targeted backoff not recorded")
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
		s := replayState(events[:n])
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
	h.add(&journal.HuntStart{Hunt: 1, Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Cores: []int{0}, Parked: []int{-10, -30, -5}, Failing: []int{-20, -30, -10}, Candidates: []int{0, 2}, Trials: 5, TrialS: 120, DurationS: 120})
	h.add(&journal.HuntGroup{Hunt: 1, Group: 1, Stage: "probe", Cores: []int{2}, Probe: &journal.CombinationMember{Core: 0, Offset: -15}, Held: []journal.CombinationMember{{Core: 2, Offset: -5}}, Profile: []int{-15, -30, -5}, DurationS: 120})
	h.start(Action{Kind: RunTrial, Trial: Trial{Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Cores: []int{0}, Profile: []int{-15, -30, -5}, DurationS: 120, Hunt: 1, Group: 1, Phase: journal.PhaseHunt, Condition: machine.Parked}})
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
	h.add(&journal.HuntStart{Hunt: 1, Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Cores: []int{0, 1}, Failing: []int{-20, -10}, Parked: []int{-10, -5}, Candidates: []int{0, 1}, Trials: 5, TrialS: 120, DurationS: 120})
	h.add(&journal.HuntGroup{Hunt: 1, Group: 1, Stage: "probe", Set: []int{0, 1}, Granularity: 2, Probe: &journal.CombinationMember{Core: 0, Offset: -15}, Held: []journal.CombinationMember{{Core: 1, Offset: -8}}, Profile: []int{-15, -8}, DurationS: 120})
	h.start(Action{Kind: RunTrial, Trial: Trial{Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Cores: []int{0, 1}, Profile: []int{-15, -8}, DurationS: 120, Hunt: 1, Group: 1, Phase: journal.PhaseHunt, Condition: machine.Parked}})
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

func TestForecastAllPassStaysWithinItsRequirement(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseSearch, offset: -20, check: true}, coreStart{phase: journal.PhaseSearch, offset: -10})
	for range 20 {
		a := h.next()
		if a.Kind == Decide {
			h.decide(a)
			continue
		}
		p := h.start(a).Data.(*journal.TrialIntent)
		f := Forecast(h.events)
		var pass *ForecastBranch
		for i := range f.Branches {
			b := &f.Branches[i]
			switch b.Premise {
			case IfPass:
				pass = b
			case IfAllPass:
				if pass != nil && pass.Next != nil && (pass.Next.Core != *p.Core || pass.Next.Regime != p.Regime) {
					t.Fatalf("all-pass branch assumed passes of another requirement first: next %+v, branch %+v", pass.Next, b)
				}
			case IfNamed, IfUnnamed, IfInconclusive:
			}
		}
		if p.Core != nil && *p.Core == 0 && pass != nil && pass.Next != nil && pass.Next.Core == 1 {
			return
		}
		h.add(&journal.TrialEnd{Trial: p.Trial, Outcome: journal.OutcomePass, DurationS: p.DurationS}, len(h.events))
	}
	t.Fatal("search never interleaved the confirming core with the searching one")
}

func TestForecastNamesCoreAtZeroSeparately(t *testing.T) {
	h := hasRoomHarness(t, -20, 0)
	h.start(Action{Kind: RunTrial, Trial: Trial{Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Cores: []int{0, 1}, DurationS: 120, Phase: journal.PhaseChecking, Condition: machine.Together}})
	var named []ForecastBranch
	for _, b := range Forecast(h.events).Branches {
		if b.Premise == IfNamed {
			named = append(named, b)
		}
	}
	if len(named) != 2 || named[0].Core == nil || *named[0].Core != 0 || named[1].Core == nil || *named[1].Core != 1 {
		t.Fatalf("named branches %+v, want core 00 standing in, then core 01 at 0", named)
	}
	if forecastDeadEnd(named[0]) || forecastDeadEnd(named[1]) || !forecastZeroRerun(named[1]) {
		t.Fatalf("outside multi-core R7, a named failure at 0 must rerun with every core at 0 before ending tuning: %+v", named)
	}
}

func forecastDeadEnd(b ForecastBranch) bool {
	return slices.ContainsFunc(b.Decisions, func(d journal.Payload) bool { _, ok := d.(*journal.DeadEnd); return ok })
}

func forecastZeroRerun(b ForecastBranch) bool {
	return b.Next != nil && b.Next.Rerun && b.Next.Condition == machine.Parked && allZero(b.Next.Profile)
}

func TestForecastR7PremisesFoldDistinctOutcomesWithoutInventedTelemetry(t *testing.T) {
	h := hasRoomHarness(t, -30, -30, 0, 0, -30, -30, -30, -30)
	h.add(&journal.HostRanking{Ranking: h.s.ids()})
	r7Fact(h, true, []int{0, 1, 2, 3}, h.s.Profile(), map[int]float64{0: 1.1, 1: 1.15, 2: 1.09, 3: 1.2}, []int{3}, nil, nil, nil)
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{machine.R7}})
	h.decide(h.s.cycleNext())
	p := h.start(h.s.cycleNext()).Data.(*journal.TrialIntent)
	plan := Forecast(h.events)
	type outcome struct {
		premise Premise
		core    int
		moved   int
		to      int
		dead    bool
	}
	want := []outcome{
		{IfPass, -1, -1, 0, false},
		{IfAllPass, -1, -1, 0, false},
		{IfNamed, 0, 0, -29, false},
		{IfNamed, 2, 1, -16, false},
		{IfNamed, 3, -1, 0, false},
		{IfUnnamed, -1, -1, 0, false},
		{IfInconclusive, -1, -1, 0, false},
	}
	var got []outcome
	for _, b := range plan.Branches {
		if b.WithoutTelemetry != (b.Premise != IfInconclusive) {
			t.Fatalf("R7 forecast lost its telemetry assumption: %+v", b)
		}
		if b.OffsetOrder {
			t.Fatalf("a measured load forecast its order from offsets: %+v", b)
		}
		o := outcome{premise: b.Premise, core: -1, moved: -1, dead: forecastDeadEnd(b)}
		if b.Core != nil {
			o.core = *b.Core
			if b.AtZero != (o.core != 0) || b.TopRequester != (o.core == 3) {
				t.Fatalf("core %02d at 0 %t, top %t: want at 0 for cores 02 and 03, top for core 03", o.core, b.AtZero, b.TopRequester)
			}
		}
		for _, d := range b.Decisions {
			switch d := d.(type) {
			case *journal.TunerDecision:
				if d.Decision == journal.Backoff {
					o.moved, o.to = d.Core, d.ToOffset
				}
			case *journal.HuntStart, *journal.HuntGroup, *journal.Combination:
				if b.Premise != IfUnnamed {
					t.Fatalf("a named multi-core R7 forecast entered a hunt: %+v", b)
				}
			}
		}
		if b.Premise == IfUnnamed && (b.Next == nil || b.Next.Condition != machine.Parked || b.Next.Hunt == 0 || !slices.Equal(b.Next.Profile, []int{-30, -30, 0, 0, 0, 0, 0, 0})) {
			t.Fatalf("an unnamed failure did not locate with CCD 1 at CO 0: %+v", b)
		}
		got = append(got, o)
		if o.dead {
			if b.Next != nil {
				t.Fatalf("dead-end branch scheduled a trial: %+v", b)
			}
		} else if b.Next == nil || b.NeedsRanking || b.NeedsHistory {
			t.Fatalf("recorded fixture did not settle: %+v", b)
		}
		if o.moved >= 0 && (!b.Next.Rerun || !slices.Equal(b.Next.Cores, p.Cores)) {
			t.Fatalf("backoff did not rerun the failed load: %+v", b)
		}
		if b.Premise == IfAllPass {
			if b.Passes != 3 || b.Next.DurationS != 300 {
				t.Fatalf("all-pass did not fulfill three short passes: %+v", b)
			}
			continue
		}
		// Fold the actual telemetry-free outcome through the tuner independently.
		s := replayState(h.events)
		end := &journal.TrialEnd{Trial: p.Trial, DurationS: p.DurationS, Outcome: journal.OutcomePass}
		switch b.Premise {
		case IfNamed:
			end.Outcome, end.Signal, end.Core = journal.OutcomeFailure, machine.ComputationError, b.Core
		case IfUnnamed:
			end.Outcome, end.Signal = journal.OutcomeFailure, machine.ComputationError
		case IfInconclusive:
			end.Outcome, end.DurationS = journal.OutcomeInconclusive, 0
		case IfPass, IfAllPass:
		}
		seq := len(h.events) + 1
		s.Fold(journal.Event{Seq: seq, Data: end})
		var decisions []journal.Payload
		var next *Trial
		for range 100 {
			a := s.Next()
			if a.Kind == ReadRanking {
				t.Fatal("unexpected ranking read with recorded ranking")
			}
			if a.Kind == RunTrial {
				trial := a.Trial
				intent, _, err := trial.Complete(0, s.Profile())
				if err != nil {
					t.Fatal(err)
				}
				trial.Workload = intent.Workload
				next = &trial
				break
			}
			decisions = append(decisions, a.Payload)
			seq++
			s.Fold(journal.Event{Seq: seq, Data: a.Payload, Cause: a.Cause})
			if _, dead := a.Payload.(*journal.DeadEnd); dead {
				break
			}
		}
		if diff := cmp.Diff(decisions, b.Decisions); diff != "" {
			t.Fatalf("%s core %d folded decisions (-tuner +forecast):\n%s", b.Premise, o.core, diff)
		}
		if diff := cmp.Diff(next, b.Next); diff != "" {
			t.Fatalf("%s core %d next trial (-tuner +forecast):\n%s", b.Premise, o.core, diff)
		}
	}
	if diff := cmp.Diff(want, got, cmp.AllowUnexported(outcome{})); diff != "" {
		t.Fatal(diff)
	}
}

func TestForecastR7UnnamedUsesOffsetFallbackWithoutRequests(t *testing.T) {
	h := hasRoomHarness(t, 0, -30, 0, 0)
	h.add(&journal.HostRanking{Ranking: h.s.ids()})
	h.start(Action{Kind: RunTrial, Trial: Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, DurationS: 120, Phase: journal.PhaseChecking, Condition: machine.Together}})
	for _, b := range Forecast(h.events).Branches {
		if b.Premise != IfUnnamed {
			continue
		}
		if forecastDeadEnd(b) || b.Next == nil || !b.Next.Rerun {
			t.Fatalf("zero top must step down to the movable loaded core: %+v", b)
		}
		if !b.OffsetOrder {
			t.Fatalf("an unmeasured load did not say offsets ordered it: %+v", b)
		}
		for _, d := range b.Decisions {
			if move, ok := d.(*journal.TunerDecision); ok && move.Decision == journal.Backoff {
				if move.Core != 1 || move.ToOffset != -29 {
					t.Fatalf("invented voltage target without telemetry: %+v", move)
				}
				return
			}
		}
		t.Fatalf("missing offset-fallback backoff: %+v", b)
	}
	t.Fatal("missing unnamed branch")
}

func TestForecastR7NamesACoreAtZeroOnEachCCD(t *testing.T) {
	h := hasRoomHarness(t, 0, 0, 0, -30)
	h.add(&journal.HostRanking{Ranking: h.s.ids()})
	all := []int{0, 1, 2, 3}
	r7Fact(h, true, all, h.s.Profile(), map[int]float64{0: 1.2, 1: 1.1, 2: 1.1, 3: 1.2}, nil, nil, nil, nil)
	h.start(Action{Kind: RunTrial, Trial: Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: all, DurationS: 120, Phase: journal.PhaseChecking, Condition: machine.Together}})
	type named struct {
		core           int
		atZero, top    bool
		dead, zeroRuns bool
		moved          int
	}
	var got []named
	for _, b := range Forecast(h.events).Branches {
		if b.Premise != IfNamed {
			continue
		}
		n := named{core: *b.Core, atZero: b.AtZero, top: b.TopRequester, dead: forecastDeadEnd(b), zeroRuns: forecastZeroRerun(b), moved: -1}
		for _, d := range b.Decisions {
			if move, ok := d.(*journal.TunerDecision); ok && move.Decision == journal.Backoff {
				n.moved = move.Core
			}
		}
		got = append(got, n)
	}
	// Core 01 at 0 routes to CCD 0, whose cores are all at 0; core 02 at 0 routes to CCD 1, whose top requester moves.
	// Neither CCD 0 failure ends tuning before its rerun with every core at 0.
	want := []named{
		{core: 3, top: true, moved: 3},
		{core: 1, atZero: true, zeroRuns: true, moved: -1},
		{core: 2, atZero: true, moved: 3},
		{core: 0, atZero: true, top: true, zeroRuns: true, moved: -1},
	}
	if diff := cmp.Diff(want, got, cmp.AllowUnexported(named{})); diff != "" {
		t.Fatalf("named branches (-want +got):\n%s", diff)
	}
}

func TestForecastR7BasisFollowsTheRunningLoadsMeasurement(t *testing.T) {
	h := hasRoomHarness(t, -30, -30, -30, -30, -30, -30, -30, -30)
	h.add(&journal.HostRanking{Ranking: h.s.ids()})
	partial := []int{0, 1, 2}
	r7Fact(h, true, partial, h.s.Profile(), map[int]float64{0: 1.1, 1: 1.2, 2: 1.0}, nil, nil, nil, nil)
	h.start(Action{Kind: RunTrial, Trial: Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: partial, DurationS: 120, Phase: journal.PhaseChecking, Condition: machine.Together}})
	for _, status := range h.s.R7Status() {
		if status.Workload == machine.Workloads(machine.R7)[0].ID && slices.Contains(partial, status.Core) && !status.OffsetFallback {
			t.Fatalf("fixture: the full part must be unmeasured: %+v", status)
		}
	}
	for _, b := range Forecast(h.events).Branches {
		if b.Premise == IfUnnamed && b.OffsetOrder {
			t.Fatalf("the partial's own measurement ordered its backoff, not offsets: %+v", b)
		}
	}
}

func TestForecastUnmeasuredChainEndingFollowsOffsetOrder(t *testing.T) {
	h := hasRoomHarness(t, -20, -20, -20, -20)
	h.add(&journal.HostRanking{Ranking: h.s.ids()})
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{machine.R7}})
	for range 20 {
		a := h.next()
		for a.Kind == Decide {
			h.decide(a)
			a = h.next()
		}
		p := h.start(a).Data.(*journal.TrialIntent)
		for _, b := range Forecast(h.events).Branches {
			if b.Premise != IfPass {
				continue
			}
			for _, d := range b.Decisions {
				if chain, ok := d.(*journal.CheckingChain); ok && len(chain.SourceSeqs) == 0 {
					// Tied offsets idle the whole two-core CCD at once, so its chain ends without any measurement.
					if len(chain.Cores) != 0 || !b.OffsetOrder {
						t.Fatalf("an unmeasured chain ending claimed earlier requests: %+v", b)
					}
					return
				}
			}
		}
		h.add(&journal.TrialEnd{Trial: p.Trial, Outcome: journal.OutcomePass, DurationS: p.DurationS})
	}
	t.Fatal("no pass forecast derived the CCD's chain")
}
