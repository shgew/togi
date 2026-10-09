package tuner

import (
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func hasRoomStarts(offsets ...int) []coreStart {
	starts := make([]coreStart, len(offsets))
	for i, x := range offsets {
		starts[i] = coreStart{phase: journal.PhaseAtLimit, offset: x, fail: new(x - 1), pass: new(x)}
	}
	return starts
}

func hasRoomHarness(t *testing.T, offsets ...int) *harness {
	t.Helper()
	h := newHarness(t, hasRoomStarts(offsets...)...)
	h.decide(h.next())
	return h
}

func TestCoveredOpenCycleYieldsToDeepening(t *testing.T) {
	offsets := []int{-49, -49, -49, -50}
	for _, tt := range []struct {
		name             string
		passedFullCycles []int
		covered          bool
		complete         bool
	}{
		{"same profile", offsets, true, false},
		{"deeper passed full-cycle profile", []int{-50, -49, -49, -50}, true, false},
		{"shallower passed full-cycle profile", []int{-48, -49, -49, -50}, false, false},
		{"completed same profile", offsets, true, true},
	} {
		t.Run(tt.name, func(t *testing.T) {
			starts := make([]coreStart, len(offsets))
			for i, v := range offsets {
				starts[i] = coreStart{phase: journal.PhaseAtLimit, offset: v}
			}
			h := newHarness(t, starts...)
			for i, pair := range [][]int{{0, 1}, {1, 2}, {2, 3}, {3, 0}, {1, 3}} {
				h.add(&journal.Combination{Combination: i + 1, Hunt: i + 1, Members: []journal.CombinationMember{{Core: pair[0], Offset: -50}, {Core: pair[1], Offset: -50}}})
			}
			h.add(&journal.ProfileChange{To: tt.passedFullCycles})
			h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: h.s.steps})
			cycle := h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleEnd, Passed: true, Full: true}).Seq
			h.add(&journal.ProfileChange{From: tt.passedFullCycles, To: offsets})
			h.add(&journal.CheckingCycle{Cycle: 2, Event: journal.CycleStart, Steps: h.s.steps})
			if tt.complete {
				// Replay a fully executed cycle before asking Next to close it.
				// The covered shortcut must only replace work still left to run.
				for {
					a := h.s.cycleNext()
					if a.Kind == RunTrial {
						h.trial(a, passed)
					} else if _, end := a.Payload.(*journal.CheckingCycle); !end {
						h.decide(a)
					} else {
						break
					}
				}
			}
			a := h.next()
			end, ok := a.Payload.(*journal.CheckingCycle)
			if tt.complete {
				if a.Kind != Decide || !ok || end.Event != journal.CycleEnd || !end.Passed || !end.Full || end.Cycle != 2 {
					t.Fatalf("completed cycle must close passed and full: %+v", a)
				}
				h.decide(a)
				nextRound(h)
				return
			}
			if got := ok && end.Event == journal.CycleEnd && !end.Passed; got != tt.covered {
				t.Fatalf("covered end %t, want %t: %+v", got, tt.covered, a)
			}
			if !tt.covered {
				if _, step := a.Payload.(*journal.CheckingStep); step {
					h.decide(a)
					a = h.next()
				}
				if a.Kind != RunTrial {
					t.Fatalf("uncovered incomplete cycle must continue testing: %+v", a)
				}
				return
			}
			if a.Cause[0] != cycle {
				t.Fatalf("cause %v, want cycle #%d first", a.Cause, cycle)
			}
			h.decide(a)
			nextRound(h)
		})
	}
}

func TestCycleCoverage(t *testing.T) {
	h := hasRoomHarness(t, -10, -12, -13, -11)
	cases := []struct {
		name    string
		steps   []machine.Regime
		missing []string
	}{{"default", config.Default().Checking.Cycle, nil}, {"short", []machine.Regime{machine.R1, machine.R7}, []string{"R1: 2 more steps", "R2: 3 more steps", "R3: 1 more steps", "R4: 1 more steps", "R5: 1 more steps", "R6: 1 more steps", "R7: 2 more steps"}}}
	for _, tt := range cases {
		t.Run(tt.name, func(t *testing.T) {
			full, missing := h.s.fullCycleCoverage(tt.steps)
			if full != (len(tt.missing) == 0) || cmp.Diff(tt.missing, missing) != "" {
				t.Fatalf("full-cycle coverage %t, missing (-want +got):\n%s", full, cmp.Diff(tt.missing, missing))
			}
		})
	}
}

func TestR7TrialsAndSharedDuration(t *testing.T) {
	h := hasRoomHarness(t, -10, -11, -12, -13)
	cfg := config.Default()
	cfg.Checking.Cycle = []machine.Regime{machine.R7}
	cfg.Durations.CheckingAllCoreS = 480
	h.add(&journal.ConfigLoaded{Config: snapshotConfig(cfg)})
	h.decide(h.next())
	h.decide(h.next())
	for _, part := range []struct {
		cores []int
		long  int
	}{
		{[]int{0, 1}, 120},
		{[]int{2, 3}, 120},
		{[]int{0, 1, 2, 3}, 240},
	} {
		for trial := range 4 {
			a := h.next()
			for a.Kind == Decide {
				if _, chain := a.Payload.(*journal.CheckingChain); !chain {
					t.Fatalf("unexpected decision between parts: %+v", a)
				}
				h.decide(a)
				a = h.next()
			}
			duration := 120
			if trial == 3 {
				duration = part.long
			}
			if a.Kind != RunTrial || a.Trial.Regime != machine.R7 || a.Trial.DurationS != duration || !slices.Equal(a.Trial.Cores, part.cores) {
				t.Fatalf("part %v trial %d: %+v", part.cores, trial, a)
			}
			h.trial(a, passed)
		}
	}
	if _, ok := h.next().Payload.(*journal.CheckingCycle); !ok {
		t.Fatal("cycle not complete")
	}
}

func TestAttributionAtGroupParkedOffsetsAndAlreadyShallower(t *testing.T) {
	h := hasRoomHarness(t, -10, -12)
	profile := []int{-10, -12}
	tr := Trial{Core: 0, Regime: machine.R6, Phase: journal.PhaseHunt, Condition: machine.Parked, Cores: []int{0, 1}, Workload: machine.Workloads(machine.R6)[0].ID, DurationS: 120, Profile: profile, Hunt: 1, Group: 1}
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(0), DurationS: 10})
	a := h.next()
	f, ok := a.Payload.(*journal.Failure)
	if !ok || *f.Offset != -10 || f.Condition != machine.Parked {
		t.Fatalf("group failure %+v", a)
	}
	h.decide(a)
	a = h.next()
	d, ok := a.Payload.(*journal.TunerDecision)
	if !ok || d.ToOffset != -9 || d.Phase != journal.PhaseHunt {
		t.Fatalf("backoff %+v", a)
	}
	h.decide(a)
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(0), DurationS: 10})
	h.decide(h.next())
	a = h.next()
	d, ok = a.Payload.(*journal.TunerDecision)
	if !ok || d.ToOffset != -9 || d.FromOffset != -9 {
		t.Fatalf("already shallow %+v", a)
	}
}

func TestRerunLongFailedPart(t *testing.T) {
	h := hasRoomHarness(t, -10, -11)
	tr := Trial{Regime: machine.R7, Cores: []int{0, 1}, Workload: machine.Workloads(machine.R7)[0].ID, Condition: machine.Together, DurationS: 600}
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(0)})
	first := h.decide(h.next())
	h.decide(h.next())
	h.add(&journal.ProfileChange{From: []int{-10, -11}, To: []int{-9, -11}})
	a := h.next()
	if a.Kind != RunTrial || !a.Trial.Rerun || a.Trial.DurationS != 120 || !slices.Equal(a.Cause, []int{first.Seq}) {
		t.Fatalf("rerun starts %+v", a)
	}
	for range h.s.n {
		h.trial(h.next(), passed)
	}
	a = h.next()
	if a.Kind != RunTrial || a.Trial.DurationS != 600 {
		t.Fatalf("long rerun %+v", a)
	}
	h.trial(a, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(0)})
	failure := h.decide(h.next())
	h.add(&journal.TunerDecision{Core: 0, Phase: journal.PhaseChecking, Decision: journal.Backoff, FromOffset: -9, ToOffset: -8, FailurePoint: new(-9)}, failure.Seq)
	h.add(&journal.ProfileChange{From: []int{-9, -11}, To: []int{-8, -11}})
	a, ok := h.s.rerunNext()
	if !ok || a.Trial.DurationS != 600 {
		t.Fatalf("passing trials lost after repeated long-class commitment: %+v", a)
	}
	if diff := cmp.Diff([]int{failure.Seq}, a.Cause); diff != "" {
		t.Fatalf("long rerun cause (-want +got):\n%s", diff)
	}
	h.trial(a, passed)
	for range h.s.n {
		a, ok = h.s.rerunNext()
		if !ok || a.Trial.DurationS != 120 {
			t.Fatalf("new obligation did not demand its passing trials: %+v", a)
		}
		h.trial(a, passed)
	}
	if a, ok = h.s.rerunNext(); ok {
		t.Fatalf("rerun repeated after both obligations completed: %+v", a)
	}
}

func TestRerunRepeatedCommitmentCitesLatestFailure(t *testing.T) {
	h := hasRoomHarness(t, -10)
	tr := Trial{Core: 0, Regime: machine.R1, Workload: machine.Workloads(machine.R1)[0].ID, Phase: journal.PhaseChecking, Condition: machine.Together, DurationS: 120}
	failed := journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(0)}
	commit := func(a Action, from, to int) int {
		h.trial(a, failed)
		failure := h.decide(h.next())
		h.add(&journal.TunerDecision{Core: 0, Phase: journal.PhaseChecking, Decision: journal.Backoff, FromOffset: from, ToOffset: to, FailurePoint: new(from)}, failure.Seq)
		h.add(&journal.ProfileChange{From: []int{from}, To: []int{to}})
		return failure.Seq
	}
	first := commit(Action{Kind: RunTrial, Trial: tr}, -10, -9)
	a, ok := h.s.rerunNext()
	if !ok || !a.Trial.Rerun {
		t.Fatalf("missing first rerun: %+v", a)
	}
	if diff := cmp.Diff([]int{first}, a.Cause); diff != "" {
		t.Fatalf("first cause (-want +got):\n%s", diff)
	}
	second := commit(a, -9, -8)
	k := classOf(h.s.intents["0001"])
	want := []rerun{{class: k, seq: first}, {class: k, seq: second}}
	if diff := cmp.Diff(want, h.s.obligations, cmp.AllowUnexported(rerun{}, trialClass{})); diff != "" {
		t.Fatalf("obligations (-want +got):\n%s", diff)
	}
	a, ok = h.s.rerunNext()
	if !ok || !a.Trial.Rerun {
		t.Fatalf("missing second rerun: %+v", a)
	}
	if diff := cmp.Diff([]int{second}, a.Cause); diff != "" {
		t.Fatalf("second cause (-want +got):\n%s", diff)
	}
	for range h.s.n {
		h.trial(a, passed)
		a, ok = h.s.rerunNext()
	}
	if ok {
		t.Fatalf("rerun repeated after the same number of passes: %+v", a)
	}
}

func TestCheckingCarriesDeeperPassAcrossBackoff(t *testing.T) {
	h := hasRoomHarness(t, -20)
	cfg := config.Default()
	cfg.Checking.Cycle = []machine.Regime{machine.R1}
	h.add(&journal.ConfigLoaded{Config: snapshotConfig(cfg)})
	h.decide(h.next())
	first := h.next()
	if first.Kind != RunTrial || first.Trial.Regime != machine.R1 {
		t.Fatalf("first requirement %+v", first)
	}
	h.trial(first, passed)
	h.add(&journal.TunerDecision{Core: 0, Phase: journal.PhaseChecking, Decision: journal.Backoff, FromOffset: -20, ToOffset: -19, FailurePoint: new(-20), Reason: "test backoff"})
	h.add(&journal.ProfileChange{From: []int{-20}, To: []int{-19}})
	if a := h.s.cycleNext(); a.Kind != Decide {
		t.Fatalf("deeper passing trial was lost after backoff: %+v", a)
	}
}

func TestTogetherSingleNonzeroAttribution(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: 0}, coreStart{phase: journal.PhaseHasRoom, offset: -12})
	h.add(&journal.ProfileChange{To: []int{0, -12}})
	tr := Trial{Regime: machine.R6, Cores: []int{0, 1}, Workload: machine.Workloads(machine.R6)[0].ID, Condition: machine.Together, Phase: journal.PhaseChecking, DurationS: 120}
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash})
	a := h.next()
	f, ok := a.Payload.(*journal.Failure)
	if !ok || f.Attribution != journal.Attributed || *f.Core != 1 || *f.Offset != -12 {
		t.Fatalf("single-nonzero attribution %+v", a)
	}
}

func TestCycleStartInvalidatesProjectedChecking(t *testing.T) {
	h := hasRoomHarness(t, -10)
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: h.s.steps})
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleEnd, Passed: true, Full: true})
	closed := projected(h)
	if closed.Checking == nil || closed.Checking.CycleOpen {
		t.Fatalf("expected closed cycle: %+v", closed.Checking)
	}
	h.add(&journal.CheckingCycle{Cycle: 2, Event: journal.CycleStart, Steps: h.s.steps})
	open := projected(h)
	if open.Checking == nil || open.Checking.Cycle != 2 || !open.Checking.CycleOpen {
		t.Fatalf("new cycle absent from projection: %+v", open.Checking)
	}
}

func TestTogetherMultipleMCECoresRemainUnattributed(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: 0}, coreStart{phase: journal.PhaseHasRoom, offset: -12})
	h.add(&journal.ProfileChange{To: []int{0, -12}})
	tr := Trial{Regime: machine.R7, Cores: []int{0, 1}, Workload: machine.Workloads(machine.R7)[0].ID, Condition: machine.Together, Phase: journal.PhaseChecking, DurationS: 120}
	intent := h.start(Action{Kind: RunTrial, Trial: tr})
	left := h.add(&journal.MCE{Core: 0, BankType: machine.LoadStore})
	right := h.add(&journal.MCE{Core: 1, BankType: machine.LoadStore})
	h.add(&journal.TrialEnd{Trial: intent.Data.(*journal.TrialIntent).Trial, Outcome: journal.OutcomeFailure, Signal: machine.Crash}, intent.Seq, left.Seq, right.Seq)
	a, ok := h.s.Attribution()
	if !ok {
		t.Fatal("no failure attribution")
	}
	f, ok := a.Payload.(*journal.Failure)
	if !ok || f.Attribution != journal.Unattributed || f.Core != nil || f.Offset != nil {
		t.Fatalf("multiple named MCE cores became attributed: %+v", a)
	}
}

func commitRerunSource(h *harness, kind string) (Trial, journal.Event) {
	h.t.Helper()
	tr := Trial{Regime: machine.R6, Cores: []int{0, 1}, Workload: machine.Workloads(machine.R6)[0].ID, Condition: machine.Together, DurationS: 600}
	var source journal.Event
	if kind == "idle" {
		source = h.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Condition: machine.Together, Profile: h.s.Profile()})
		tr.Regime, tr.Workload, tr.Cores, tr.DurationS = machine.R6, machine.Workloads(machine.R6)[0].ID, h.s.ids(), h.s.durations.CheckingIdleS
	} else {
		end := journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 5}
		if kind == "attributed" {
			end.Core = new(0)
		}
		h.trial(Action{Kind: RunTrial, Trial: tr}, end)
		source = h.decide(h.next())
	}
	if kind != "attributed" {
		start := h.decide(h.s.huntStartNext()).Data.(*journal.HuntStart)
		if kind == "direct" {
			tr.Workload, tr.Cores, tr.DurationS, tr.Condition = machine.Workloads(machine.R6)[0].ID, h.s.ids(), 240, machine.Parked
			tr.Hunt, tr.Group = start.Hunt, 1
			h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(1)})
			source = h.decide(h.next())
			h.decide(h.next())
		} else {
			result, cores := "culprit", []int{0}
			if kind == "combination" {
				result, cores = "combination", []int{0, 1}
			}
			h.add(&journal.HuntEnd{Hunt: start.Hunt, Result: result, Cores: cores}, start.Failure)
			if kind == "combination" {
				h.decide(h.next())
			}
		}
	}
	move := h.decide(h.next())
	if p, ok := move.Data.(*journal.TunerDecision); !ok || p.Decision != journal.Backoff {
		h.t.Fatalf("missing commitment: %+v", move)
	}
	h.add(&journal.ProfileChange{From: h.s.Profile(), To: h.s.offsets()}, move.Seq)
	if projected(h).Hunt != nil {
		h.t.Fatal("committed hunt remains active")
	}
	assertProjectionReplay(h)
	return tr, source
}

func TestRerunDerivedCommitments(t *testing.T) {
	for _, kind := range []string{"attributed", "idle", "culprit", "combination", "direct"} {
		t.Run(kind, func(t *testing.T) {
			h := hasRoomHarness(t, -10, -11, -12, -13)
			tr, source := commitRerunSource(h, kind)
			durations := make([]int, h.s.n)
			for i := range durations {
				durations[i] = h.s.durations.ShortTrialS
			}
			if tr.DurationS != h.s.durations.ShortTrialS {
				durations = append(durations, tr.DurationS)
			}
			for _, duration := range durations {
				a, ok := h.s.rerunNext()
				if !ok || !a.Trial.Rerun || a.Trial.Condition != machine.Together || a.Trial.Regime != tr.Regime || a.Trial.Workload != tr.Workload || !slices.Equal(a.Trial.Cores, tr.Cores) || a.Trial.DurationS != duration {
					t.Fatalf("derived %s rerun: %+v", kind, a)
				}
				if diff := cmp.Diff([]int{source.Seq}, a.Cause); diff != "" {
					t.Fatalf("rerun cause (-want +got):\n%s", diff)
				}
				h.trial(a, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: duration})
			}
			if a, ok := h.s.rerunNext(); ok {
				t.Fatalf("completed obligation repeats: %+v", a)
			}
		})
	}
}

func TestRerunFIFOAndSharedDuration(t *testing.T) {
	h := hasRoomHarness(t, -10, -11, -12, -13)
	var causes []int
	for _, tc := range []struct {
		core     int
		regime   machine.Regime
		duration int
	}{{0, machine.R1, 120}, {1, machine.R2, 600}} {
		tr := Trial{Core: tc.core, Regime: tc.regime, Workload: machine.Workloads(tc.regime)[1].ID, Condition: machine.Together, DurationS: tc.duration}
		h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(tc.core)})
		failure := h.decide(h.next())
		causes = append(causes, failure.Seq)
		move := h.decide(h.next())
		h.add(&journal.ProfileChange{From: h.s.Profile(), To: h.s.offsets()}, move.Seq)
	}
	for i, regime := range []machine.Regime{machine.R1, machine.R2} {
		count := h.s.n
		if i == 1 {
			count++
		}
		for trial := range count {
			a, ok := h.s.rerunNext()
			duration := 120
			if trial == h.s.n {
				duration = 600
			}
			if !ok || a.Trial.Core != i || a.Trial.Regime != regime || a.Trial.Workload != machine.Workloads(regime)[1].ID || a.Trial.DurationS != duration || !slices.Equal(a.Cause, []int{causes[i]}) {
				t.Fatalf("FIFO obligation %d trial %d: %+v", i, trial, a)
			}
			h.trial(a, passed)
		}
	}
	if a, ok := h.s.rerunNext(); ok {
		t.Fatalf("shared duration added redundant long trial: %+v", a)
	}
}

func TestRepeatedCycleClassesAddTrials(t *testing.T) {
	for _, regime := range machine.Regimes {
		t.Run(string(regime), func(t *testing.T) {
			h := hasRoomHarness(t, -10)
			catalog := len(machine.Workloads(regime))
			steps := make([]machine.Regime, catalog+1)
			for i := range steps {
				steps[i] = regime
			}
			h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: steps})
			first := h.s.requirements(0)
			last := h.s.requirements(catalog)
			for i, q := range last {
				if q.count != 2*first[i].count || q.class != first[i].class {
					t.Fatalf("repeated class did not add requirements: first %+v, last %+v", first, last)
				}
			}
		})
	}
}

func TestDrainStopsAtTrialBoundary(t *testing.T) {
	h := newHarness(t, searchAt(-10)...)
	h.trial(h.next(), failed)
	a, ok := h.s.Drain()
	if !ok {
		t.Fatal("drain did not attribute completed failure")
	}
	f, yes := a.Payload.(*journal.Failure)
	if !yes || f.Core == nil || *f.Core != 0 {
		t.Fatalf("drain attribution: %+v", a)
	}
	h.decide(a)
	a, ok = h.s.Drain()
	back, yes := a.Payload.(*journal.TunerDecision)
	if !ok || !yes || back.ToOffset != -5 || back.FailurePoint == nil || *back.FailurePoint != -10 {
		t.Fatalf("drain backoff: %+v", a)
	}
	h.decide(a)
	if a, ok := h.s.Drain(); ok {
		t.Fatalf("drain scheduled new work: %+v", a)
	}
	if a := h.next(); a.Kind != RunTrial || a.Trial.Offset != -5 {
		t.Fatalf("next lost search after drain: %+v", a)
	}
}

func TestDrainRejectsUnattributedFailureAtZero(t *testing.T) {
	h := hasRoomHarness(t, 0, 0)
	failure := h.add(&journal.Failure{Attribution: journal.Unattributed, Condition: machine.Together, Profile: []int{0, 0}, Signal: machine.Crash})
	a, ok := h.s.Drain()
	p, yes := a.Payload.(*journal.DeadEnd)
	if !ok || !yes || p.Condition != journal.DeadEndFailureAtZero || p.Core != nil || cmp.Diff([]int{failure.Seq}, a.Cause) != "" {
		t.Fatalf("drain missed all-zero dead end: %+v", a)
	}
}
