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

func huntHarness(t *testing.T, cores int, duration int) *harness {
	t.Helper()
	starts := make([]coreStart, cores)
	for i := range starts {
		starts[i] = coreStart{phase: journal.PhaseAtLimit, offset: -30, fail: new(-31)}
	}
	h := newHarness(t, starts...)
	h.decide(h.next())
	ids := h.s.ids()
	tr := Trial{Regime: machine.R6, Cores: ids, Workload: machine.Workloads(machine.R6)[0].ID, Condition: machine.Together, Phase: journal.PhaseChecking, DurationS: duration, Profile: h.s.offsets()}
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 5})
	a := h.next()
	if f, ok := a.Payload.(*journal.Failure); !ok || f.Attribution != journal.Unattributed {
		t.Fatalf("source attribution %+v", a)
	}
	h.decide(a)
	h.decide(driveToHuntStart(h))
	return h
}

// driveToHuntStart decides whatever precedes the hunt and returns the undecided hunt.start.
func driveToHuntStart(h *harness) Action {
	h.t.Helper()
	for range 50 {
		a := h.next()
		if _, ok := a.Payload.(*journal.HuntStart); ok {
			return a
		}
		if a.Kind != Decide {
			h.t.Fatalf("expected hunt start, got %+v", a)
		}
		h.decide(a)
	}
	h.t.Fatal("hunt not started")
	return Action{}
}

func runGroup(h *harness, a Action, fail bool) {
	h.t.Helper()
	if a.Kind != RunTrial || a.Trial.Condition != machine.Parked {
		h.t.Fatalf("group trial %+v", a)
	}
	end := passed
	if fail {
		end = journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 5}
	}
	h.trial(a, end)
	if fail {
		attribution := h.next()
		if f, ok := attribution.Payload.(*journal.Failure); !ok || f.Attribution != journal.Unattributed {
			h.t.Fatalf("unexpected group attribution %+v", attribution)
		}
		h.decide(attribution)
	}
}

func assertHuntNextReplay(h *harness, want Action, next func(*State) Action, failure string) {
	h.t.Helper()
	replayed := replayState(h.events)
	if diff := cmp.Diff(want, next(replayed)); diff != "" {
		h.t.Fatalf("%s:\n%s", failure, diff)
	}
}

func TestHuntParkedOffsetsRaisesThePassedFullCycleProfile(t *testing.T) {
	for _, tt := range []struct {
		name                      string
		passedFullCycles, failing []int
		parked                    []int
		fromCycle                 bool
		candidates                []int
	}{
		{"shallower everywhere", []int{-10, -10, -10, -10}, []int{-12, -10, -10, -10}, []int{-10, -10, -10, -10}, true, []int{0}},
		{"a yielded core takes its failing offset", []int{-10, -10, -10, -10}, []int{-12, -8, -10, -10}, []int{-10, -8, -10, -10}, true, []int{0}},
		{"deeper everywhere falls back to all-zero", []int{-12, -12, -12, -12}, []int{-10, -10, -10, -10}, []int{0, 0, 0, 0}, false, []int{0, 1, 2, 3}},
	} {
		t.Run(tt.name, func(t *testing.T) {
			starts := make([]coreStart, 4)
			for i := range starts {
				starts[i] = coreStart{phase: journal.PhaseAtLimit, offset: tt.failing[i]}
			}
			h := newHarness(t, starts...)
			h.add(&journal.ProfileChange{To: tt.passedFullCycles})
			h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{machine.R1}})
			passedCycle := h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleEnd, Passed: true, Full: true})
			h.add(&journal.ProfileChange{From: tt.passedFullCycles, To: tt.failing})
			tr := Trial{Regime: machine.R6, Cores: h.s.ids(), Workload: machine.Workloads(machine.R6)[0].ID, Condition: machine.Together, Phase: journal.PhaseChecking, DurationS: 120, Profile: tt.failing}
			h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 5})
			attribution := h.next()
			if f, ok := attribution.Payload.(*journal.Failure); !ok || f.Attribution != journal.Unattributed {
				t.Fatalf("source attribution %+v", attribution)
			}
			h.decide(attribution)
			p := driveToHuntStart(h).Payload.(*journal.HuntStart)
			wantSeq := 0
			if tt.fromCycle {
				wantSeq = passedCycle.Seq
			}
			got := struct {
				Parked     []int
				ParkedSeq  int
				Candidates []int
			}{p.Parked, p.ParkedSeq, p.Candidates}
			want := struct {
				Parked     []int
				ParkedSeq  int
				Candidates []int
			}{tt.parked, wantSeq, tt.candidates}
			if diff := cmp.Diff(want, got); diff != "" {
				t.Fatalf("hunt.start (-want +got):\n%s", diff)
			}
		})
	}
}

func TestHuntParkedOffsetsSelectionAndReset(t *testing.T) {
	h := huntHarness(t, 4, 120)
	start := h.s.hunt.start
	if start.ParkedSeq != 0 || !slices.Equal(start.Parked, []int{0, 0, 0, 0}) {
		t.Fatalf("all-zero parked offsets %+v", start)
	}
	h.add(&journal.CommandReset{Core: new(0)})
	a := h.next()
	end, ok := a.Payload.(*journal.HuntEnd)
	if !ok || end.Result != "cancelled" {
		t.Fatalf("reset did not cancel hunt: %+v", a)
	}
	h.decide(a)
	if h.s.hunt != nil || len(h.s.queue) != 1 || h.s.queue[0].seq != start.Failure || h.s.queue[0].class.regime != machine.R6 {
		t.Fatalf("source not requeued with class after cancellation: %+v", h.s.queue)
	}
}

func TestAllZeroHuntParkedOffsetsOmitsZeroCause(t *testing.T) {
	h := huntHarness(t, 4, 120)
	for _, e := range h.events {
		p, ok := e.Data.(*journal.HuntStart)
		if !ok {
			continue
		}
		if p.ParkedSeq != 0 {
			t.Fatalf("parked offsets seq %d, want all-zero parked offsets", p.ParkedSeq)
		}
		if diff := cmp.Diff([]int{p.Failure}, e.Cause); diff != "" {
			t.Fatalf("hunt cause (-want +got):\n%s", diff)
		}
		return
	}
	t.Fatal("no hunt.start")
}

func TestHuntParkedOffsetsSkipsExactPassedFullCycleProfile(t *testing.T) {
	starts := make([]coreStart, 2)
	for i := range starts {
		starts[i] = coreStart{phase: journal.PhaseAtLimit, offset: -30, fail: new(-31)}
	}
	h := newHarness(t, starts...)
	h.add(&journal.ProfileChange{To: []int{-20, -20}})
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: h.s.steps})
	old := h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleEnd, Passed: true, Full: true})
	h.add(&journal.ProfileChange{From: []int{-20, -20}, To: []int{-30, -30}})
	h.add(&journal.CheckingCycle{Cycle: 2, Event: journal.CycleStart, Steps: h.s.steps})
	h.add(&journal.CheckingCycle{Cycle: 2, Event: journal.CycleEnd, Passed: true, Full: true})
	failure := h.add(&journal.Failure{Attribution: journal.Unattributed, Signal: machine.Crash, Condition: machine.Together, Regime: machine.R6, Profile: []int{-30, -30}})
	a := h.s.huntStartNext()
	start, ok := a.Payload.(*journal.HuntStart)
	if !ok || start.Failure != failure.Seq || start.ParkedSeq != old.Seq || cmp.Diff([]int{-20, -20}, start.Parked) != "" {
		t.Fatalf("parked offsets %+v, want older #%d", a, old.Seq)
	}
}

func TestHuntSkipsPreviouslyReachedFailure(t *testing.T) {
	h := huntHarness(t, 2, 120)
	failure := h.add(&journal.Failure{Attribution: journal.Unattributed, Condition: machine.Together, Signal: machine.Crash, Profile: h.s.Profile()})
	for range 2 {
		h.decide(h.next())
		for range h.s.n {
			runGroup(h, h.next(), false)
		}
	}
	h.decide(h.next())
	h.decide(h.next())
	h.decide(h.next())
	h.add(&journal.ProfileChange{From: h.s.Profile(), To: h.s.offsets()})
	a := h.s.huntStartNext()
	p, ok := a.Payload.(*journal.HuntSkipped)
	if !ok || p.Failure != failure.Seq || !strings.Contains(p.Reason, "combination C1") || cmp.Diff([]int{failure.Seq}, a.Cause) != "" {
		t.Fatalf("marked source was hunted: %+v", a)
	}
	h.decide(a)
	if len(h.s.queue) != 0 || h.s.hunt != nil {
		t.Fatal("skipped source remains pending")
	}
}

func TestHuntLoadedIdleSplit(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -30}, coreStart{phase: journal.PhaseAtLimit, offset: -30}, coreStart{phase: journal.PhaseAtLimit, offset: -30}, coreStart{phase: journal.PhaseAtLimit, offset: -30})
	h.decide(h.next())
	tr := Trial{Regime: machine.R6, Cores: []int{0, 2}, Workload: machine.Workloads(machine.R6)[0].ID, Condition: machine.Together, Phase: journal.PhaseChecking, DurationS: 120, Profile: []int{-30, -30, -30, -30}}
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 5})
	h.decide(h.next())
	h.decide(driveToHuntStart(h))
	var round []*journal.HuntGroup
	for range 50 {
		a := h.next()
		if a.Kind != Decide {
			runGroup(h, a, false)
			continue
		}
		if g, ok := a.Payload.(*journal.HuntGroup); ok {
			if len(round) > 0 && (g.Granularity != 2 || g.Stage != round[0].Stage || g.DurationS != round[0].DurationS || g.Escalated != round[0].Escalated || !slices.Equal(g.Set, round[0].Set)) {
				break
			}
			round = append(round, g)
		} else if len(round) > 0 {
			break
		}
		h.decide(a)
	}
	if len(round) == 0 || round[len(round)-1].Index+1 != len(round) {
		t.Fatalf("first binary round never completed: %+v", round)
	}
	var parts [][]int
	for i, g := range round {
		if g.Granularity != 2 || g.Stage != "part" || g.Index != i {
			t.Fatalf("group %d is not part %d of the binary round: %+v", g.Group, i, g)
		}
		parts = append(parts, g.Cores)
	}
	if diff := cmp.Diff([][]int{{0, 2}, {1, 3}}, parts); diff != "" {
		t.Fatalf("first split into loaded and idle cores (-want +got):\n%s", diff)
	}
}

func TestNextHuntReusesEstablishedPartGroups(t *testing.T) {
	h := huntHarness(t, 4, 120)
	fails := func(p []int) bool { return p[0] <= -28 && p[1] <= -28 }
	var first []*journal.HuntGroup
	var second []*journal.HuntGroup
	hunts := 1
	for range 400 {
		a := h.next()
		switch p := a.Payload.(type) {
		case *journal.HuntStart:
			hunts++
			h.decide(a)
			continue
		case *journal.HuntGroup:
			if hunts == 1 {
				first = append(first, p)
			} else {
				second = append(second, p)
			}
			h.decide(a)
			continue
		}
		if a.Kind == RunTrial && a.Trial.Rerun {
			h.trial(a, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 5})
			continue
		}
		if a.Kind == RunTrial {
			if hunts == 2 {
				break
			}
			runGroup(h, a, fails(a.Trial.Profile))
			continue
		}
		h.decide(a)
	}
	if hunts != 2 {
		t.Fatalf("hunts %d, want a second hunt after the failed rerun", hunts)
	}
	ran := func(m *journal.HuntGroup) bool { return m.Inferred == "" && !m.Skipped }
	idle := slices.IndexFunc(first, func(m *journal.HuntGroup) bool { return slices.Equal(m.Cores, []int{2, 3}) && ran(m) })
	if idle < 0 {
		t.Fatalf("first hunt never ran the part of cores 02 and 03: %+v", first)
	}
	again := slices.IndexFunc(second, func(m *journal.HuntGroup) bool { return slices.Equal(m.Cores, []int{2, 3}) })
	if again < 0 || second[again].Inferred != "pass" || !slices.Equal(second[again].Profile, first[idle].Profile) {
		t.Fatalf("second hunt groups %+v, want the part of cores 02 and 03 inferred from the first hunt's passes", second)
	}
}

func TestHuntGroupReuseAfterReset(t *testing.T) {
	for _, stage := range []string{"part", "complement"} {
		for _, tt := range []struct {
			name       string
			pass       bool
			afterReset bool
			inferred   string
		}{
			{"old failure", false, false, ""},
			{"old pass", true, false, ""},
			{"new failure", false, true, "failure"},
			{"new pass", true, true, "pass"},
		} {
			t.Run(stage+"/"+tt.name, func(t *testing.T) {
				h := newHarness(t, searchAt(-30, -30, -30, -30)...)
				profile := []int{-30, -30, 0, 0}
				tr := Trial{Regime: machine.R6, Cores: h.s.ids(), Workload: machine.Workloads(machine.R6)[0].ID, DurationS: 120, Condition: machine.Parked, Profile: profile}
				reset := func() {
					h.add(&journal.Combination{Combination: 1, Hunt: 1, Members: []journal.CombinationMember{{Core: 0, Offset: -30}, {Core: 1, Offset: -30}}})
					h.add(&journal.CommandReset{Core: new(0)})
					h.add(&journal.CorePhase{Core: 0, To: journal.PhaseSearch, Offset: -10, ClearedCombination: []int{1}})
				}
				if tt.afterReset {
					reset()
				}
				end, trials := passed, 5
				if !tt.pass {
					end, trials = journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash}, 1
				}
				for range trials {
					h.trial(Action{Kind: RunTrial, Trial: tr}, end)
				}
				if !tt.afterReset {
					reset()
				}
				h.add(&journal.HuntStart{Hunt: 2, Regime: tr.Regime, Workload: tr.Workload, Cores: tr.Cores, DurationS: 120, TrialS: 120, Trials: 5, Failing: []int{-30, -30, -30, -30}, Parked: []int{0, 0, 0, 0}, Candidates: h.s.ids()})
				a := h.s.planGroup(h.s.hunt, groupPlan{set: h.s.ids(), cores: []int{0, 1}, g: 2, stage: stage, duration: 120}, "testing")
				m := a.Payload.(*journal.HuntGroup)
				got := struct {
					Inferred string
					Skipped  bool
				}{m.Inferred, m.Skipped}
				want := struct {
					Inferred string
					Skipped  bool
				}{tt.inferred, false}
				if diff := cmp.Diff(want, got); diff != "" {
					t.Fatalf("group after reset (-want +got):\n%s", diff)
				}
			})
		}
	}
}

func TestRunningHuntGroupReusesEarlierPasses(t *testing.T) {
	for _, stage := range []string{"part", "complement"} {
		t.Run(stage, func(t *testing.T) {
			h := newHarness(t, searchAt(-30, -30, -30, -30)...)
			tr := Trial{Regime: machine.R6, Cores: h.s.ids(), Workload: machine.Workloads(machine.R6)[0].ID, DurationS: 120, Condition: machine.Parked, Profile: []int{-30, -30, 0, 0}}
			for range 4 {
				h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
			}
			h.add(&journal.HuntStart{Hunt: 2, Regime: tr.Regime, Workload: tr.Workload, Cores: tr.Cores, DurationS: 120, TrialS: 120, Trials: 5, Failing: []int{-30, -30, -30, -30}, Parked: []int{0, 0, 0, 0}, Candidates: h.s.ids()})
			a := h.s.planGroup(h.s.hunt, groupPlan{set: h.s.ids(), cores: []int{0, 1}, g: 2, stage: stage, duration: 120}, "testing")
			h.decide(a)
			for _, tt := range []struct {
				name    string
				passes  int
				outcome string
				advance bool
			}{
				{"before new start", 4, "running", false},
				{"after new start", 5, "pass", true},
			} {
				t.Run(tt.name, func(t *testing.T) {
					if tt.advance {
						tr.Hunt, tr.Group = 2, 1
						h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
					}
					p := h.s.projectHunt()
					_, advance := h.s.nextGroupPlan(h.s.hunt)
					got := struct {
						Passes  int
						Outcome string
						Advance bool
					}{p.Groups[0].Passes, p.Groups[0].Outcome, advance}
					want := struct {
						Passes  int
						Outcome string
						Advance bool
					}{tt.passes, tt.outcome, tt.advance}
					if diff := cmp.Diff(want, got); diff != "" {
						t.Fatalf("running group (-want +got):\n%s", diff)
					}
				})
			}
		})
	}
}

func TestHuntGroupDoesNotReuseShallowerOrIncomparablePasses(t *testing.T) {
	for _, tt := range []struct {
		name    string
		profile []int
	}{
		{"shallower", []int{-29, -30, 0, 0}},
		{"incomparable", []int{-31, -29, 0, 0}},
	} {
		t.Run(tt.name, func(t *testing.T) {
			h := newHarness(t, searchAt(-30, -30, -30, -30)...)
			tr := Trial{Regime: machine.R6, Cores: h.s.ids(), Workload: machine.Workloads(machine.R6)[0].ID, DurationS: 120, Condition: machine.Parked, Profile: tt.profile}
			for range 5 {
				h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
			}
			h.add(&journal.HuntStart{Hunt: 2, Regime: tr.Regime, Workload: tr.Workload, Cores: tr.Cores, DurationS: 120, TrialS: 120, Trials: 5, Failing: []int{-30, -30, -30, -30}, Parked: []int{0, 0, 0, 0}, Candidates: h.s.ids()})
			h.decide(h.s.planGroup(h.s.hunt, groupPlan{set: h.s.ids(), cores: []int{0, 1}, g: 2, stage: "part", duration: 120}, "testing"))
			p := h.s.projectHunt()
			got := struct {
				Passes  int
				Outcome string
			}{p.Groups[0].Passes, p.Groups[0].Outcome}
			want := struct {
				Passes  int
				Outcome string
			}{0, "running"}
			if diff := cmp.Diff(want, got); diff != "" {
				t.Fatalf("mismatched profile (-want +got):\n%s", diff)
			}
		})
	}
}

func TestHuntSkipsReachedGroup(t *testing.T) {
	h := huntHarness(t, 4, 120)
	h.add(&journal.Combination{Combination: 1, Hunt: 999, Members: []journal.CombinationMember{{Core: 2, Offset: -30}, {Core: 3, Offset: -30}}})
	a := h.next()
	m, ok := a.Payload.(*journal.HuntGroup)
	if !ok || !m.Skipped {
		t.Fatalf("group on newly marked profile %+v", a)
	}
	h.decide(a)
	if next := h.next(); next.Kind == RunTrial && next.Trial.Group == m.Group {
		t.Fatalf("ran skipped group %+v", next)
	}
}

func TestRepeatedParkedCoreProbeReturnsToBinaryPartsAfterPass(t *testing.T) {
	starts := make([]coreStart, 16)
	for i := range starts {
		starts[i] = coreStart{phase: journal.PhaseAtLimit, offset: -10, fail: new(-11)}
	}
	h := newHarness(t, starts...)
	c := config.Default()
	c.Checking.Cycle = []machine.Regime{machine.R6}
	h.add(&journal.ConfigLoaded{Path: config.DefaultPath, Config: snapshotConfig(c)})
	var prior []int
	for range 1000 {
		a := h.next()
		if m, ok := a.Payload.(*journal.HuntGroup); ok && m.Hunt == 3 && m.Group == 1 {
			if diff := cmp.Diff([]int{3}, m.Cores); diff != "" {
				t.Fatalf("corroborated core was not probed first (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff(append([]int{h.s.hunt.seq}, prior...), a.Cause); diff != "" {
				t.Fatalf("probe lost its evidence (-want +got):\n%s", diff)
			}
			singleton := func(when string) {
				t.Helper()
				parts := h.s.HuntPlan().Parts
				if len(parts) != 1 || !slices.Equal(parts[0].Failing, []int{3}) {
					t.Fatalf("%s, the dashboard plan must show the corroborated core alone: %+v", when, parts)
				}
			}
			singleton("before the singleton is recorded")
			h.decide(a)
			singleton("while the singleton runs")
			for range h.s.n {
				trial := h.next()
				h.trial(trial, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: trial.Trial.DurationS})
			}
			normal := h.next()
			p, ok := normal.Payload.(*journal.HuntGroup)
			if !ok || p.Granularity != 2 || p.Index != 0 || p.Inferred != "" || p.Profile[3] != -8 || p.Profile[4] != -10 {
				t.Fatalf("a passing singleton must not establish its untested binary group: %+v", normal)
			}
			assertHuntNextReplay(h, normal, (*State).Next, "resumed probe fallback changed (-want +got)")
			h.decide(normal)
			if parts := h.s.HuntPlan().Parts; len(parts) != 2 {
				t.Fatalf("after the singleton passes the plan returns to halves: %+v", parts)
			}
			return
		}
		if a.Kind == Decide {
			e := h.decide(a)
			if f, ok := e.Data.(*journal.Failure); ok && f.Condition == machine.Parked && f.Attribution == journal.Attributed && f.Core != nil && *f.Core == 3 {
				prior = append(prior, e.Seq)
			}
			continue
		}
		if a.Kind != RunTrial {
			t.Fatalf("unexpected action: %+v", a)
		}
		intent := h.start(a)
		p := intent.Data.(*journal.TrialIntent)
		end := &journal.TrialEnd{Trial: p.Trial, Outcome: journal.OutcomePass, DurationS: p.DurationS}
		if p.Profile[3] < -8 || p.Profile[3] < 0 && p.Profile[4] < -9 {
			end.Outcome, end.Signal = journal.OutcomeFailure, machine.Crash
		}
		h.add(end, intent.Seq)
	}
	t.Fatal("corroborated singleton probe was never scheduled")
}

func TestRepeatedParkedCoreProbeRequiresMatchingAdjacentFailuresSinceReset(t *testing.T) {
	for _, tt := range []struct {
		name   string
		change func(*Trial, *journal.Failure)
		reset  bool
		probe  bool
	}{
		{name: "matching adjacent failures", probe: true},
		{name: "different workload", change: func(tr *Trial, _ *journal.Failure) { tr.Workload = machine.Workloads(machine.R6)[1].ID }},
		{name: "different duration", change: func(tr *Trial, _ *journal.Failure) { tr.DurationS = 600 }},
		{name: "different loaded cores", change: func(tr *Trial, _ *journal.Failure) { tr.Cores = []int{0, 1} }},
		{name: "together failure", change: func(tr *Trial, f *journal.Failure) { tr.Condition, f.Condition = machine.Together, machine.Together }},
		{name: "unattributed failure", change: func(_ *Trial, f *journal.Failure) { f.Attribution, f.Core, f.Offset = journal.Unattributed, nil, nil }},
		{name: "different culprit", change: func(_ *Trial, f *journal.Failure) { f.Core = new(0) }},
		{name: "nonadjacent offsets", change: func(_ *Trial, f *journal.Failure) { f.Offset = new(-8) }},
		{name: "reset between failures", reset: true},
	} {
		t.Run(tt.name, func(t *testing.T) {
			starts := make([]coreStart, 4)
			for i, offset := range []int{-20, -8, -20, -20} {
				starts[i] = coreStart{phase: journal.PhaseAtLimit, offset: offset}
			}
			h := newHarness(t, starts...)
			h.decide(h.next())
			var prior []int
			for i, offset := range []int{-10, -9} {
				if i == 1 && tt.reset {
					h.add(&journal.CommandReset{Core: new(1)})
				}
				tr := Trial{Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Cores: h.s.ids(), Condition: machine.Parked, Phase: journal.PhaseHunt, DurationS: 120, Profile: []int{0, offset, 0, 0}}
				failure := &journal.Failure{Attribution: journal.Attributed, Core: new(1), Offset: new(offset), Condition: machine.Parked, Regime: machine.R6, Profile: tr.Profile}
				if i == 1 && tt.change != nil {
					tt.change(&tr, failure)
				}
				intent, end := h.trial(Action{Kind: RunTrial, Trial: tr}, failed)
				failure.Trial = intent.Data.(*journal.TrialIntent).Trial
				prior = append(prior, h.add(failure, end.Seq).Seq)
			}
			h.add(&journal.CorePhase{Core: 1, To: journal.PhaseAtLimit, Offset: -8, FailurePoint: new(-9)})
			tr := Trial{Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Cores: h.s.ids(), Condition: machine.Together, Phase: journal.PhaseChecking, DurationS: 120, Profile: []int{-20, -8, -20, -20}}
			intent, end := h.trial(Action{Kind: RunTrial, Trial: tr}, failed)
			failure := h.add(&journal.Failure{Trial: intent.Data.(*journal.TrialIntent).Trial, Attribution: journal.Unattributed, Condition: machine.Together, Regime: machine.R6, Profile: tr.Profile}, end.Seq)
			h.decide(h.s.huntStartNext())
			plan, ok := h.s.nextGroupPlan(h.s.hunt)
			if !ok {
				t.Fatal("hunt did not schedule a group")
			}
			want := []int{0, 1}
			if tt.probe {
				want = []int{1}
			}
			if diff := cmp.Diff(want, plan.cores); diff != "" {
				t.Fatalf("initial group for failure #%d (-want +got):\n%s", failure.Seq, diff)
			}
			if tt.probe {
				action := h.s.planGroup(h.s.hunt, plan, "partition")
				if diff := cmp.Diff(append([]int{h.s.hunt.seq}, prior...), action.Cause); diff != "" {
					t.Fatalf("probe cause (-want +got):\n%s", diff)
				}
			}
		})
	}
}

func TestHuntPairAndCommitmentResume(t *testing.T) {
	h := huntHarness(t, 4, 120)
	var probes []journal.CombinationMember
	var end *journal.HuntEnd
	for range 200 {
		assertHuntNextReplay(h, h.s.Next(), (*State).Next, "hunt prefix replay (-live +replay)")
		a := h.next()
		if group, ok := a.Payload.(*journal.HuntGroup); ok {
			h.decide(a)
			if group.Group == 1 && !slices.Equal(group.Cores, []int{2, 3}) {
				t.Fatalf("recent failure point did not move its part first: %v", group.Cores)
			}
			if group.Probe != nil {
				probes = append(probes, *group.Probe)
			}
			continue
		}
		if a.Kind == RunTrial {
			p := a.Trial.Profile
			runGroup(h, a, p[0] <= -30 && p[1] <= -20 && p[2] == 0 && p[3] == 0)
			continue
		}
		if e, ok := a.Payload.(*journal.HuntEnd); ok {
			end = e
			h.decide(a)
			break
		}
		t.Fatalf("unexpected action %+v", a)
	}
	if end == nil || end.Result != "combination" || cmp.Diff([]int{0, 1}, end.Cores) != "" {
		t.Fatalf("hunt result %+v", end)
	}
	if diff := cmp.Diff([]journal.CombinationMember{{Core: 0, Offset: -30}, {Core: 1, Offset: -20}}, end.Members); diff != "" {
		t.Errorf("probed members (-want +got):\n%s", diff)
	}
	want := []journal.CombinationMember{{Core: 0, Offset: -29}}
	for _, offset := range []int{-29, -28, -26, -22, -14, -18, -20, -19} {
		want = append(want, journal.CombinationMember{Core: 1, Offset: offset})
	}
	if diff := cmp.Diff(want, probes); diff != "" {
		t.Errorf("member probes (-want +got):\n%s", diff)
	}
	combination := h.next()
	p, ok := combination.Payload.(*journal.Combination)
	if !ok || cmp.Diff([]journal.CombinationMember{{Core: 0, Offset: -30}, {Core: 1, Offset: -20}}, p.Members) != "" {
		t.Fatalf("combination %+v", combination)
	}
	h.decide(combination)
	assertHuntNextReplay(h, h.s.Next(), (*State).Next, "commitment resume (-live +replay)")
	back := h.next()
	d, ok := back.Payload.(*journal.TunerDecision)
	if !ok || d.Decision != journal.Backoff || d.Phase != journal.PhaseHunt || d.Core != 0 || d.ToOffset != -29 {
		t.Fatalf("commitment %+v", d)
	}
	h.decide(back)
	if h.s.hunt != nil {
		t.Fatal("hunt reopened after commitment")
	}
	if _, reached := h.s.Reaches(h.s.offsets()); reached {
		t.Fatalf("commitment reached combination: %v", h.s.offsets())
	}
}

func TestCombinationBackoffMovesToATestedProbe(t *testing.T) {
	h := huntHarness(t, 4, 120)
	fails := func(p []int) bool {
		return p[0] <= -27 && p[1] <= -27 || p[0] == -26 && p[1] == -30
	}
	probes := map[int]int{}
	for range 200 {
		a := h.next()
		if group, ok := a.Payload.(*journal.HuntGroup); ok {
			e := h.decide(a)
			if group.Probe != nil {
				probes[e.Seq] = group.Probe.Offset
			}
			continue
		}
		if a.Kind == RunTrial {
			runGroup(h, a, fails(a.Trial.Profile))
			continue
		}
		if _, ok := a.Payload.(*journal.HuntEnd); ok {
			h.decide(a)
			break
		}
		t.Fatalf("unexpected action %+v", a)
	}
	combination := h.next()
	p, ok := combination.Payload.(*journal.Combination)
	if !ok || cmp.Diff([]journal.CombinationMember{{Core: 0, Offset: -26}, {Core: 1, Offset: -30}}, p.Members) != "" {
		t.Fatalf("combination %+v", combination)
	}
	h.decide(combination)
	back := h.next()
	d, ok := back.Payload.(*journal.TunerDecision)
	if !ok || d.Decision != journal.Backoff || d.Core != 0 || d.ToOffset != -25 {
		t.Fatalf("commitment %+v, want core 00 to -25, where its probe passed with core 01 at -30", back)
	}
	if len(back.Cause) != 2 || probes[back.Cause[1]] != -25 {
		t.Fatalf("commitment cause %v, want the combination and the passing probe", back.Cause)
	}
	h.decide(back)
	if fails(h.s.offsets()) {
		t.Fatalf("together %v still fails; one count on core 01 would have left it failing", h.s.offsets())
	}
}

func TestHuntFallbackAndFullCheck(t *testing.T) {
	for _, tc := range []struct {
		name      string
		duration  int
		fullFails bool
		result    string
		reason    []string
	}{
		{"fallback", 120, false, "fallback", []string{"unresolved", "combination"}},
		{"full failure", 600, true, "combination", []string{"member probes", "core 00 -30", "core 01 -30", "a count shallower"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := huntHarness(t, 2, tc.duration)
			full := 0
			for range 100 {
				a := h.next()
				if group, ok := a.Payload.(*journal.HuntGroup); ok {
					if group.Stage == "full" {
						full++
					}
					h.decide(a)
					continue
				}
				if a.Kind == RunTrial {
					fail := tc.fullFails && slices.Equal(a.Trial.Profile, []int{-30, -30})
					runGroup(h, a, fail)
					continue
				}
				if end, ok := a.Payload.(*journal.HuntEnd); ok {
					if end.Result != tc.result {
						t.Fatalf("result %s (%s), want %s", end.Result, end.Reason, tc.result)
					}
					if missing := missingTokens(end.Reason, tc.reason...); len(missing) > 0 {
						t.Fatalf("hunt end reason %q lacks %q", end.Reason, missing)
					}
					if tc.fullFails && full != 1 {
						t.Fatalf("full group recorded %d times", full)
					}
					return
				}
				t.Fatalf("unexpected action %+v", a)
			}
			t.Fatal("hunt did not end")
		})
	}
}

func TestHuntCombinationAlreadyBroken(t *testing.T) {
	h := hasRoomHarness(t, -29, -30)
	start := &journal.HuntStart{Hunt: 1, Failure: 10, Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Cores: []int{0, 1}, DurationS: 120, Failing: []int{-30, -30}, Parked: []int{0, 0}, Candidates: []int{0, 1}, Trials: 5, TrialS: 120}
	h.add(start)
	h.add(&journal.HuntEnd{Hunt: 1, Result: "combination", Cores: []int{0, 1}, Groups: 2})
	a := h.next()
	combination, ok := a.Payload.(*journal.Combination)
	if !ok || len(missingTokens(combination.Reason, "no backoff", "core 00", "already shallower")) > 0 {
		t.Fatalf("unneeded backoff %+v", a)
	}
	h.decide(a)
	if h.s.hunt != nil {
		t.Fatal("combination commitment did not close when profile already breaks it")
	}
}

func TestHuntCulpritDiscardsContradictedPass(t *testing.T) {
	h := hasRoomHarness(t, -29, -30)
	h.add(&journal.HuntStart{Hunt: 1, Failure: 10, Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Cores: []int{0, 1}, DurationS: 120, Failing: []int{-29, -30}, Parked: []int{0, 0}, Candidates: []int{0, 1}, Trials: 5, TrialS: 120})
	h.add(&journal.HuntEnd{Hunt: 1, Result: "culprit", Cores: []int{0}, Groups: 2})
	a := h.next()
	d, ok := a.Payload.(*journal.TunerDecision)
	if !ok || d.Phase != journal.PhaseHunt || d.Core != 0 || d.Pass != nil || d.FailurePoint == nil || *d.FailurePoint != -29 {
		t.Fatalf("culprit commitment kept contradicted pass: %+v", a)
	}
	if missing := missingTokens(d.Reason, "-29", "discarded", "contradicts"); len(missing) > 0 {
		t.Fatalf("culprit commitment reason %q lacks %q", d.Reason, missing)
	}
}

func TestSixteenCoreCulpritAndPair(t *testing.T) {
	for _, tc := range []struct {
		name    string
		members []int
		result  string
	}{
		{"single core", []int{13}, "direct"},
		{"combination pair", []int{3, 11}, "combination"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := huntHarness(t, 16, 120)
			ended := false
			for range 2500 {
				a := h.next()
				if group, ok := a.Payload.(*journal.HuntGroup); ok {
					h.decide(a)
					if group.Group > 194 {
						t.Fatalf("hunt exceeded progress bound: %d groups", group.Group)
					}
					continue
				}
				if a.Kind == RunTrial {
					fail := true
					for _, member := range tc.members {
						fail = fail && a.Trial.Profile[member] == -30
					}
					outcome := passed
					if fail {
						outcome = journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 5}
					}
					h.trial(a, outcome)
					if fail {
						attribution := h.next()
						if _, ok := attribution.Payload.(*journal.Failure); !ok {
							t.Fatalf("group failure did not produce attribution: %+v", attribution)
						}
						h.decide(attribution)
					}
					continue
				}
				if end, ok := a.Payload.(*journal.HuntEnd); ok {
					if end.Result != tc.result || cmp.Diff(tc.members, end.Cores) != "" {
						t.Fatalf("hunt result %+v, want %s %v", end, tc.result, tc.members)
					}
					h.decide(a)
					ended = true
					break
				}
				t.Fatalf("unexpected hunt action %+v", a)
			}
			if !ended {
				t.Fatal("sixteen-core hunt did not terminate")
			}
		})
	}
}

func TestHuntDurationPriorRespectsEvidenceAndShortFailures(t *testing.T) {
	for _, tt := range []struct {
		name               string
		count, workload    int
		profile            []int
		shortFailed, reset bool
		want               int
	}{
		{"valid deeper passes", 5, 0, []int{-32, -32}, false, false, 600},
		{"short failure in another workload", 5, 0, []int{-32, -32}, true, false, 120},
		{"insufficient trials", 4, 0, []int{-32, -32}, false, false, 120},
		{"different workload", 5, 1, []int{-32, -32}, false, false, 120},
		{"incomparable profile", 5, 0, []int{-32, -29}, false, false, 120},
		{"reset evidence boundary", 5, 0, []int{-32, -32}, false, true, 120},
	} {
		t.Run(tt.name, func(t *testing.T) {
			h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -30}, coreStart{phase: journal.PhaseAtLimit, offset: -30})
			h.decide(h.next())
			tr := Trial{Regime: machine.R6, Cores: h.s.ids(), Workload: machine.Workloads(machine.R6)[tt.workload].ID, Condition: machine.Parked, Phase: journal.PhaseHunt, DurationS: 120, Profile: tt.profile}
			if tt.shortFailed {
				bad := tr
				bad.Workload, bad.Profile = machine.Workloads(machine.R6)[1].ID, []int{-40, 0}
				h.trial(Action{Kind: RunTrial, Trial: bad}, failed)
				h.decide(h.next())
				h.decide(h.next())
			}
			var seqs []int
			for range tt.count {
				_, end := h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
				seqs = append(seqs, end.Seq)
			}
			if tt.reset {
				h.add(&journal.CommandReset{Core: new(0)})
			}
			tr.Workload = machine.Workloads(machine.R6)[0].ID
			tr.DurationS, tr.Profile, tr.Condition, tr.Phase = 600, []int{-30, -30}, machine.Together, journal.PhaseChecking
			h.trial(Action{Kind: RunTrial, Trial: tr}, failed)
			h.decide(h.next())
			h.decide(h.s.huntStartNext())
			plan, ok := h.s.nextGroupPlan(h.s.hunt)
			if !ok || plan.duration != tt.want {
				t.Fatalf("initial hunt duration: %+v, want %d", plan, tt.want)
			}
			a := h.s.planGroup(h.s.hunt, plan, "partition")
			if tt.want == 600 {
				if diff := cmp.Diff(append([]int{h.s.hunt.seq}, seqs...), a.Cause); diff != "" {
					t.Fatalf("duration evidence cause (-want +got):\n%s", diff)
				}
			}
			h.decide(a)
			next, _ := h.s.huntNext()
			if a.Payload.(*journal.HuntGroup).Inferred == "" && (next.Kind != RunTrial || next.Trial.DurationS != tt.want) {
				t.Fatalf("parked trial duration: %+v", next)
			}
			assertHuntNextReplay(h, next, func(s *State) Action {
				a, _ := s.huntNext()
				return a
			}, "duration resume (-live +replayed)")
		})
	}
}

func TestHuntDurationEscalationOnlyForLongerFailure(t *testing.T) {
	for _, tt := range []struct {
		name     string
		duration int
		groups   []string
	}{
		{"shorter", 90, []string{"part/120s/false", "part/120s/false"}},
		{"equal", 120, []string{"part/120s/false", "part/120s/false"}},
		{"longer", 600, []string{"part/120s/false", "part/120s/false", "full/120s/false", "part/600s/true", "part/600s/true"}},
	} {
		t.Run(tt.name, func(t *testing.T) {
			h := huntHarness(t, 2, tt.duration)
			var groups []string
			trials := 0
			for range 100 {
				a, drained := h.s.Drain()
				if !drained {
					a = h.next()
				}
				if group, ok := a.Payload.(*journal.HuntGroup); ok {
					groups = append(groups, fmt.Sprintf("%s/%ds/%t", group.Stage, group.DurationS, group.Escalated))
					h.decide(a)
					continue
				}
				if a.Kind == RunTrial {
					if a.Trial.DurationS != h.s.hunt.groups[len(h.s.hunt.groups)-1].payload.DurationS {
						t.Fatalf("trial does not use its group's duration: %+v", a)
					}
					trials++
					runGroup(h, a, false)
					continue
				}
				if end, ok := a.Payload.(*journal.HuntEnd); ok {
					if !drained || end.Result != "fallback" {
						t.Fatalf("drain did not finish the passing hunt: %+v", a)
					}
					if diff := cmp.Diff(tt.groups, groups); diff != "" {
						t.Fatalf("hunt duration sequence (-want +got):\n%s", diff)
					}
					if want := len(tt.groups) * h.s.n; trials != want {
						t.Fatalf("hunt ran %d trials, want %d", trials, want)
					}
					replayed, ok := replayState(h.events).Drain()
					if !ok {
						t.Fatal("replayed hunt did not drain")
					}
					if diff := cmp.Diff(a, replayed); diff != "" {
						t.Fatalf("resumed hunt end (-live +replayed):\n%s", diff)
					}
					h.decide(a)
					return
				}
				t.Fatalf("unexpected hunt action: %+v", a)
			}
			t.Fatal("hunt did not end")
		})
	}
}

func TestHuntEscalationAndInferredGroup(t *testing.T) {
	h := huntHarness(t, 2, 600)
	first := h.next()
	group, ok := first.Payload.(*journal.HuntGroup)
	if !ok {
		t.Fatalf("first group %+v", first)
	}
	profile := slices.Clone(group.Profile)
	tr := Trial{Regime: machine.R6, Cores: h.s.ids(), Workload: machine.Workloads(machine.R6)[0].ID, Condition: machine.Parked, DurationS: 120, Profile: profile}
	for range h.s.n {
		h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
	}
	first = h.next()
	group, ok = first.Payload.(*journal.HuntGroup)
	if !ok || group.Inferred != "pass" {
		t.Fatalf("inferred group %+v", first)
	}
	h.decide(first)
	escalated := false
	for range 200 {
		a := h.next()
		if group, ok := a.Payload.(*journal.HuntGroup); ok {
			if group.Escalated {
				escalated = true
				if group.DurationS != 600 {
					t.Fatalf("escalated duration %d", group.DurationS)
				}
			}
			h.decide(a)
			continue
		}
		if a.Kind == RunTrial {
			runGroup(h, a, false)
			continue
		}
		if _, ok := a.Payload.(*journal.HuntEnd); ok {
			if !escalated {
				t.Fatal("full pass never escalated")
			}
			return
		}
		t.Fatalf("unexpected action %+v", a)
	}
	t.Fatal("escalated hunt did not finish")
}

func TestActiveHuntProjection(t *testing.T) {
	for _, tc := range []struct {
		name      string
		stage     string
		prior     int
		newPasses int
		fail      bool
		inferred  string
		skipped   bool
		reset     bool
		escalated bool
		probe     *journal.CombinationMember
		held      []journal.CombinationMember
		passes    int
		outcome   string
	}{
		{name: "running reuses earlier valid passes", stage: "part", prior: 2, newPasses: 1, passes: 3, outcome: "running"},
		{name: "passing complement accumulates trials", stage: "complement", prior: 4, newPasses: 1, passes: 5, outcome: "pass"},
		{name: "failure invalidates earlier passes", stage: "part", prior: 2, fail: true, outcome: "failure"},
		{name: "inferred pass", stage: "part", prior: 5, inferred: "pass", passes: 5, outcome: "pass"},
		{name: "inferred failure", stage: "complement", inferred: "failure", outcome: "failure"},
		{name: "skipped", stage: "part", skipped: true, outcome: "skipped"},
		{name: "reset excludes reused evidence", stage: "part", prior: 4, reset: true, newPasses: 1, passes: 1, outcome: "running"},
		{name: "escalated full uses only new trials", stage: "full", prior: 4, newPasses: 1, escalated: true, passes: 1, outcome: "running"},
		{name: "member probe preserves held members", stage: "probe", prior: 4, newPasses: 1, probe: &journal.CombinationMember{Core: 0, Offset: -30}, held: []journal.CombinationMember{{Core: 1, Offset: -30}}, passes: 1, outcome: "running"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := huntHarness(t, 4, 600)
			old := h.s.hunt.start
			duration := 120
			if tc.escalated {
				duration = 600
			}
			tr := Trial{Regime: old.Regime, Workload: old.Workload, Cores: old.Cores, Condition: machine.Parked, DurationS: duration, Profile: []int{-30, -30, 0, 0}, Hunt: 2, Group: 1}
			for range tc.prior {
				h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
			}
			if tc.reset {
				h.add(&journal.CommandReset{Core: new(0)})
			}
			start := &journal.HuntStart{Hunt: 2, Failure: old.Failure, Trial: old.Trial, Regime: old.Regime, Workload: old.Workload, Cores: old.Cores, DurationS: 600, Trials: 5, TrialS: 120, Parked: []int{0, 0, 0, 0}, Failing: []int{-30, -30, -30, -30}, Candidates: h.s.ids()}
			begin := h.add(start)
			group := h.add(&journal.HuntGroup{Hunt: 2, Group: 1, Cores: []int{0, 1}, Profile: tr.Profile, Stage: tc.stage, DurationS: duration, Inferred: tc.inferred, Skipped: tc.skipped, Escalated: tc.escalated, Probe: tc.probe, Held: tc.held})
			for range tc.newPasses {
				h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
			}
			if tc.fail {
				h.trial(Action{Kind: RunTrial, Trial: tr}, failed)
			}
			want := &journal.HuntState{Hunt: 2, Seq: begin.Seq, Failure: start.Failure, Regime: start.Regime, Trial: start.Trial, Parked: start.Parked, Candidates: start.Candidates, Escalated: tc.escalated, Groups: []journal.GroupState{{Group: 1, Seq: group.Seq, Cores: []int{0, 1}, Probe: tc.probe, Held: tc.held, Outcome: tc.outcome, Passes: tc.passes, Needed: 5}}}
			st := projected(h)
			if st.Phase != string(journal.PhaseHunt) {
				t.Fatalf("active hunt phase %s", st.Phase)
			}
			if diff := cmp.Diff(want, st.Hunt); diff != "" {
				t.Fatalf("hunt projection (-want +got):\n%s", diff)
			}
			assertProjectionReplay(h)
			h.add(&journal.HuntEnd{Hunt: 2, Result: "cancelled"}, begin.Seq)
			if projected(h).Hunt != nil {
				t.Fatal("cancelled hunt remains active")
			}
			assertProjectionReplay(h)
		})
	}
}

func TestDrainCommitsResolvedHuntWithoutStartingGroups(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -30, fail: new(-31)}, coreStart{phase: journal.PhaseAtLimit, offset: -30, fail: new(-31)})
	h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "old", Trial: "source", Seq: 2}, Class: journal.TrialClass{Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Cores: []int{0, 1}, DurationS: 120}, Condition: machine.Parked, Core: new(1), Profile: []int{0, -30}, Outcome: journal.OutcomeFailure, Signal: machine.Crash})
	h.decide(h.next())
	h.trial(Action{Kind: RunTrial, Trial: Trial{Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Cores: []int{0, 1}, Condition: machine.Together, Phase: journal.PhaseChecking, DurationS: 600}}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash})
	h.decide(h.next())
	h.decide(h.next())
	if a, ok := h.s.Drain(); ok {
		t.Fatalf("drain planned new group: %+v", a)
	}
	h.decide(h.next())
	a, ok := h.s.Drain()
	end, yes := a.Payload.(*journal.HuntEnd)
	if !ok || !yes || end.Result != "culprit" || cmp.Diff([]int{1}, end.Cores) != "" {
		t.Fatalf("drain did not resolve singleton: %+v", a)
	}
	h.decide(a)
	a, ok = h.s.Drain()
	back, yes := a.Payload.(*journal.TunerDecision)
	if !ok || !yes || back.Decision != journal.Backoff || back.Core != 1 || back.ToOffset != -29 {
		t.Fatalf("drain did not commit culprit: %+v", a)
	}
	h.decide(a)
	if a, ok := h.s.Drain(); ok {
		t.Fatalf("drain ran work after commitment: %+v", a)
	}
}

func TestDrainRecordsFallbackCombinationBeforeBackoff(t *testing.T) {
	h := huntHarness(t, 2, 120)
	for range 2 {
		h.decide(h.next())
		for range h.s.n {
			runGroup(h, h.next(), false)
		}
	}
	a, ok := h.s.Drain()
	end, yes := a.Payload.(*journal.HuntEnd)
	if !ok || !yes || end.Result != "fallback" {
		t.Fatalf("drain did not close unresolved hunt: %+v", a)
	}
	h.decide(a)
	a, ok = h.s.Drain()
	combination, yes := a.Payload.(*journal.Combination)
	if !ok || !yes || !combination.Fallback || cmp.Diff([]journal.CombinationMember{{Core: 0, Offset: -30}, {Core: 1, Offset: -30}}, combination.Members) != "" {
		t.Fatalf("drain lost fallback combination: %+v", a)
	}
	h.decide(a)
	a, ok = h.s.Drain()
	back, yes := a.Payload.(*journal.TunerDecision)
	if !ok || !yes || back.Decision != journal.Backoff || back.ToOffset != -29 {
		t.Fatalf("drain did not break combination: %+v", a)
	}
	h.decide(a)
	if _, reached := h.s.Reaches(h.s.offsets()); reached {
		t.Fatal("drain left together offsets reaching combination")
	}
}

func TestLocatedHuntResumesDuringLocate(t *testing.T) {
	h := r7Harness(t)
	failEscalatedR7(h, journal.TrialEnd{DurationS: 41, TopRequesters: []int{0}})
	for range 2 {
		h.decide(h.next())
	}
	for range 2 {
		h.trial(h.next(), passed)
	}
	want := h.s.Next()
	if want.Kind != RunTrial || want.Trial.Group != 1 || !slices.Equal(want.Trial.Profile, []int{-30, -30, 0, 0}) {
		t.Fatalf("locate did not continue: %+v", want)
	}
	assertHuntNextReplay(h, want, (*State).Next, "replay changed the locate trial")
	if got := projected(h).Hunt.Groups[0]; got.Passes != 2 || got.Outcome != "running" {
		t.Fatalf("locate progress after resume: %+v", got)
	}
	assertProjectionReplay(h)
}

func TestParkedFailureNamingACoreAtZeroRerunsAllZero(t *testing.T) {
	for _, rerunFails := range []bool{false, true} {
		t.Run(fmt.Sprint(rerunFails), func(t *testing.T) {
			h := huntHarness(t, 4, 120)
			a := h.next()
			group, ok := a.Payload.(*journal.HuntGroup)
			if !ok {
				t.Fatalf("first group %+v", a)
			}
			h.decide(a)
			parked := slices.IndexFunc(h.s.ids(), func(id int) bool { return !slices.Contains(group.Cores, id) })
			intent := h.start(h.next())
			mce := h.add(&journal.MCE{Core: parked, BankType: machine.LoadStore})
			h.add(&journal.TrialEnd{Trial: intent.Data.(*journal.TrialIntent).Trial, Outcome: journal.OutcomeFailure, Signal: machine.Crash}, intent.Seq, mce.Seq)
			a = h.next()
			if f, ok := a.Payload.(*journal.Failure); !ok || f.Core == nil || *f.Core != parked || *f.Offset != 0 {
				t.Fatalf("attribution %+v", a)
			}
			failure := h.decide(a)
			a = h.next()
			want := Trial{Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Cores: h.s.ids(), DurationS: 120, Condition: machine.Parked, Phase: journal.PhaseHunt, Profile: []int{0, 0, 0, 0}, Rerun: true}
			want.Requirement = ScheduledRequirement{Kind: "zero-rerun", Class: ScheduledClass{Regime: want.Regime, Workload: want.Workload, Cores: coresKey(want.Cores), DurationS: want.DurationS}, Since: failure.Seq, Rule: rerunEvidence, Needed: 1}
			if diff := cmp.Diff(Action{Kind: RunTrial, Trial: want, Cause: []int{failure.Seq}}, a); diff != "" {
				t.Fatalf("all-zero rerun (-want +got):\n%s", diff)
			}
			if !rerunFails {
				h.trial(a, passed)
				a = h.next()
				next, ok := a.Payload.(*journal.HuntGroup)
				if !ok || len(next.Cores) == 0 || slices.ContainsFunc(next.Cores, func(id int) bool { return !slices.Contains(group.Cores, id) }) {
					t.Fatalf("the failure did not go to group %d's members %v: %+v", group.Group, group.Cores, a)
				}
				assertProjectionReplay(h)
				return
			}
			_, rerun := h.trial(a, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash})
			h.decide(h.next())
			a = h.next()
			dead, ok := a.Payload.(*journal.DeadEnd)
			if !ok || dead.Condition != journal.DeadEndFailureAtZero || !slices.Equal(a.Cause, []int{failure.Seq, rerun.Seq}) {
				t.Fatalf("dead end %+v", a)
			}
			want2 := fmt.Sprintf("core %02d failed at CO 0; the rerun with every core at CO 0 failed too (#%d), so the instability is not caused by Curve Optimizer", parked, rerun.Seq)
			if diff := cmp.Diff(want2, dead.Detail); diff != "" {
				t.Fatal(diff)
			}
			assertProjectionReplay(h)
		})
	}
}

func TestTogetherFailureNamingACoreAtZeroIsHuntedAfterAPassedRerun(t *testing.T) {
	h := hasRoomHarness(t, 0, -10)
	tr := Trial{Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Phase: journal.PhaseChecking, Condition: machine.Together, DurationS: 120, Cores: []int{0, 1}}
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(0)})
	failure := h.decide(h.next())
	h.trial(h.next(), passed)
	a := h.next()
	start, ok := a.Payload.(*journal.HuntStart)
	if !ok || start.Failure != failure.Seq || !slices.Equal(start.Candidates, []int{1}) {
		t.Fatalf("the failure was not hunted among the nonzero cores: %+v", a)
	}
	h.decide(a)
	a = h.next()
	if end, ok := a.Payload.(*journal.HuntEnd); !ok || end.Result != "culprit" || !slices.Equal(end.Cores, []int{1}) {
		t.Fatalf("hunt end %+v", a)
	}
	assertProjectionReplay(h)
}
