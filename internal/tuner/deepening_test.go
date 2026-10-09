package tuner

import (
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func cleanCycleHarness(t *testing.T, offsets []int, combinations [][]int) *harness {
	t.Helper()
	starts := make([]coreStart, len(offsets))
	for i, v := range offsets {
		starts[i] = coreStart{phase: journal.PhaseAtLimit, offset: v}
	}
	h := newHarness(t, starts...)
	for i, pair := range combinations {
		h.add(&journal.Combination{Combination: i + 1, Hunt: i + 1, Members: []journal.CombinationMember{{Core: pair[0], Offset: -50}, {Core: pair[1], Offset: -50}}})
	}
	h.add(&journal.ProfileChange{To: slices.Clone(offsets)})
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: h.s.steps})
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleEnd, Passed: true, Full: true})
	return h
}

func nextRound(h *harness) *journal.DeepeningRound {
	h.t.Helper()
	for range 20 {
		a := h.next()
		if p, ok := a.Payload.(*journal.DeepeningRound); ok && p.Event == journal.CycleStart {
			h.decide(a)
			return p
		}
		if a.Kind != Decide {
			h.t.Fatalf("expected deepening round: %+v", a)
		}
		h.decide(a)
	}
	h.t.Fatal("no deepening round")
	return nil
}

func TestIdleFailureEndsDeepeningBeforeMoves(t *testing.T) {
	h := cleanCycleHarness(t, []int{-49, -49, -49, -50}, [][]int{{0, 1}, {1, 2}, {2, 3}, {3, 0}, {1, 3}})
	round := nextRound(h)
	failure := h.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Regime: machine.R6, Condition: machine.Together, Profile: h.s.Profile()})
	a := h.next()
	end, ok := a.Payload.(*journal.DeepeningRound)
	if !ok || end.Round != round.Round || end.Event != journal.CycleEnd || end.Reason != "a failure needs a hunt" {
		t.Fatalf("idle failure did not end deepening round: %+v", a)
	}
	if diff := cmp.Diff([]int{failure.Seq}, a.Cause); diff != "" {
		t.Fatalf("round end cause (-want +got):\n%s", diff)
	}
}

func TestDeepeningGlobalOptimumAndResume(t *testing.T) {
	combinations := [][]int{{0, 1}, {1, 2}, {2, 3}, {3, 0}, {1, 3}}
	h := cleanCycleHarness(t, []int{-49, -49, -49, -50}, combinations)
	r := nextRound(h)
	if diff := cmp.Diff([]int{-50, -49, -50, -49}, r.Target); diff != "" {
		t.Fatalf("global optimum (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff(r.Target, r.Profile); diff != "" {
		t.Fatalf("proposed profile (-want +got):\n%s", diff)
	}
	decisions := []journal.Decision{journal.Yield, journal.Deepen, journal.Deepen}
	for _, want := range decisions {
		live := h.next()
		replayed := replayState(h.events)
		if diff := cmp.Diff(live, replayed.Next()); diff != "" {
			t.Fatalf("round resume (-live +replayed):\n%s", diff)
		}
		d, ok := live.Payload.(*journal.TunerDecision)
		if !ok || d.Decision != want {
			t.Fatalf("move %+v, want %s", live, want)
		}
		h.decide(live)
	}
	for range 15 {
		a := h.next()
		if _, ok := a.Payload.(*journal.ProfileChange); ok {
			h.decide(a)
			break
		}
		if a.Kind != Decide {
			t.Fatalf("expected profile change: %+v", a)
		}
		h.decide(a)
	}
	if diff := cmp.Diff(r.Profile, h.s.Profile()); diff != "" {
		t.Fatalf("profile (-want +got):\n%s", diff)
	}
	for range 100 {
		replayed := replayState(h.events)
		if diff := cmp.Diff(h.s.Next(), replayed.Next()); diff != "" {
			t.Fatalf("deepening prefix replay (-live +replayed):\n%s", diff)
		}
		a := h.next()
		if a.Kind == RunTrial {
			if a.Trial.Round != r.Round {
				t.Fatalf("trial lost round: %+v", a.Trial)
			}
			h.trial(a, passed)
			continue
		}
		if end, ok := a.Payload.(*journal.DeepeningRound); ok && end.Event == journal.CycleEnd {
			if !end.Passed {
				t.Fatalf("failed round %+v", end)
			}
			h.decide(a)
			return
		}
		t.Fatalf("unexpected check action %+v", a)
	}
	t.Fatal("round checks never finished")
}

func TestDeepeningHalfwayTowardFailurePoint(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseHasRoom, offset: -10, fail: new(-50)})
	h.add(&journal.ProfileChange{To: []int{-10}})
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: h.s.steps})
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleEnd, Passed: true, Full: true})
	r := nextRound(h)
	if diff := cmp.Diff([]int{-30}, r.Profile); diff != "" {
		t.Fatalf("first halfway (-want +got):\n%s", diff)
	}
	h.decide(h.next())
	h.decide(h.next())
	for range 30 {
		a := h.next()
		if a.Kind == RunTrial {
			h.trial(a, passed)
			continue
		}
		if p, ok := a.Payload.(*journal.DeepeningRound); ok && p.Event == journal.CycleEnd {
			h.decide(a)
			break
		}
		t.Fatalf("unexpected halfway action %+v", a)
	}
	next := nextRound(h)
	if diff := cmp.Diff([]int{-40}, next.Profile); diff != "" {
		t.Fatalf("second halfway (-want +got):\n%s", diff)
	}
}

func TestActiveDeepeningProjection(t *testing.T) {
	for _, close := range []string{"passed", "cancelled"} {
		t.Run(close, func(t *testing.T) {
			h := newHarness(t, coreStart{phase: journal.PhaseHasRoom, offset: -10}, coreStart{phase: journal.PhaseHasRoom, offset: -10}, coreStart{phase: journal.PhaseHasRoom, offset: -10}, coreStart{phase: journal.PhaseHasRoom, offset: -10})
			h.add(&journal.ProfileChange{To: []int{-10, -10, -10, -10}})
			round := &journal.DeepeningRound{Round: 2, Event: journal.CycleStart, Target: []int{-20, -9, -10, -10}, Profile: []int{-15, -9, -10, -10}, Cores: []int{0, 1}, Trials: 2, TrialS: 120}
			begin := h.add(round)
			want := &journal.DeepeningState{Round: 2, Seq: begin.Seq, Target: round.Target, Profile: round.Profile, Cores: round.Cores, Checks: []journal.CheckState{
				{Regime: machine.R1, Workload: machine.Workloads(machine.R1)[1].ID, Cores: []int{0}, Needed: 2},
				{Regime: machine.R2, Workload: machine.Workloads(machine.R2)[1].ID, Cores: []int{0}, Needed: 2},
				{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, Needed: 2},
				{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[1].ID, Cores: []int{0, 1}, Needed: 2},
				{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[2].ID, Cores: []int{0, 1}, Needed: 2},
			}}
			assert := func() {
				t.Helper()
				st := projected(h)
				if st.Phase != string(journal.PhaseDeepening) {
					t.Fatalf("active round phase %s", st.Phase)
				}
				if diff := cmp.Diff(want, st.Deepening); diff != "" {
					t.Fatalf("round projection (-want +got):\n%s", diff)
				}
				assertProjectionReplay(h)
			}
			assert()
			for {
				a, ok := h.s.roundMoves()
				if !ok {
					break
				}
				h.decide(a)
				assert()
			}
			h.add(&journal.ProfileChange{From: h.s.Profile(), To: round.Profile}, begin.Seq)
			for i := range want.Checks {
				for range 2 {
					a := h.s.roundCheck()
					q := want.Checks[i]
					if a.Kind != RunTrial || a.Trial.Regime != q.Regime || a.Trial.Workload != q.Workload || a.Trial.Round != 2 || a.Trial.DurationS != 120 {
						t.Fatalf("check %d: %+v", i, a)
					}
					h.trial(a, passed)
					want.Checks[i].Passes++
					assert()
					if close == "cancelled" {
						h.add(&journal.DeepeningRound{Round: 2, Event: journal.CycleEnd, Reason: "a failure needs a hunt"}, begin.Seq)
						if projected(h).Deepening != nil {
							t.Fatal("cancelled round remains active")
						}
						assertProjectionReplay(h)
						return
					}
				}
			}
			end := h.s.roundCheck()
			p, ok := end.Payload.(*journal.DeepeningRound)
			if !ok || p.Event != journal.CycleEnd || !p.Passed || !slices.Equal(end.Cause, []int{begin.Seq}) {
				t.Fatalf("completed round: %+v", end)
			}
			h.decide(end)
			if projected(h).Deepening != nil {
				t.Fatal("completed round remains active")
			}
			assertProjectionReplay(h)
		})
	}
}

func TestLiveDeepeningFailureEndsRoundBeforeBackoff(t *testing.T) {
	h := cleanCycleHarness(t, []int{-10, -10}, nil)
	r := nextRound(h)
	for {
		a, ok := h.s.roundMoves()
		if !ok {
			break
		}
		h.decide(a)
	}
	h.add(&journal.ProfileChange{From: h.s.Profile(), To: r.Profile})
	h.trial(h.s.roundCheck(), journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(0)})
	failure := h.decide(h.next())
	a := h.next()
	p, ok := a.Payload.(*journal.DeepeningRound)
	if !ok || p.Round != r.Round || p.Event != journal.CycleEnd || p.Passed || cmp.Diff([]int{failure.Seq}, a.Cause) != "" {
		t.Fatalf("live failure did not close round: %+v", a)
	}
	h.decide(a)
	a = h.next()
	back, ok := a.Payload.(*journal.TunerDecision)
	if !ok || back.Phase != journal.PhaseDeepening || back.ToOffset != r.Profile[0]+1 || back.FailurePoint == nil || *back.FailurePoint != r.Profile[0] {
		t.Fatalf("live deepening failure lost failure point: %+v", a)
	}
}
