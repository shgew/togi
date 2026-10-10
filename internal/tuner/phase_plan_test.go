package tuner

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
)

func assertPlan(t *testing.T, h *harness, want journal.PhasesState) {
	t.Helper()
	if diff := cmp.Diff(&want, h.s.PhasePlan()); diff != "" {
		t.Fatalf("phase plan (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff(h.s.PhasePlan(), replayState(h.events).PhasePlan()); diff != "" {
		t.Fatalf("phase plan after replay (-live +replayed):\n%s", diff)
	}
	assertProjectionReplay(h)
}

func endPhase1(h *harness, profile ...int) journal.Event {
	h.add(&journal.ProfileChange{To: profile})
	startCycle(h, 1)
	return endCycle(h, 1)
}

func applyRound(h *harness) {
	h.t.Helper()
	for {
		a := h.next()
		if d, ok := a.Payload.(*journal.TunerDecision); !ok || d.Decision != journal.Deepen {
			return
		}
		h.decide(a)
	}
}

func TestPhasePlan(t *testing.T) {
	t.Run("phase 1 has no candidates", func(t *testing.T) {
		h := newHarness(t, soloCore(-10, -13))
		assertPlan(t, h, journal.PhasesState{Phase: 1})
	})
	t.Run("a passed cycle that is not full does not end phase 1", func(t *testing.T) {
		h := newHarness(t, soloCore(-10, -13))
		h.add(&journal.ProfileChange{To: []int{-10}})
		startCycle(h, 1)
		h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleEnd, Passed: true})
		assertPlan(t, h, journal.PhasesState{Phase: 1})
	})
	t.Run("no core can move leaves the confirmation cycle", func(t *testing.T) {
		h := newHarness(t, soloCore(-10, -10), soloCore(-12, -12))
		end := endPhase1(h, -10, -12)
		assertPlan(t, h, journal.PhasesState{Phase: 2, Phase1End: end.Seq, Confirming: true})
	})
	t.Run("rounds left are the largest gap, not the sum", func(t *testing.T) {
		h := newHarness(t, soloCore(-10, -13), soloCore(-8, -9))
		end := endPhase1(h, -10, -8)
		assertPlan(t, h, journal.PhasesState{Phase: 2, Phase1End: end.Seq, RoundsLeft: 3, Candidates: []journal.CandidateState{
			{Core: 0, Offset: -10, SoloLimit: -13, Gap: 3},
			{Core: 1, Offset: -8, SoloLimit: -9, Gap: 1},
		}})
	})
	t.Run("a failure point excludes the counts it blocks", func(t *testing.T) {
		h := newHarness(t, coreStart{from: journal.PhaseSearch, phase: journal.PhaseHasRoom, offset: -10, pass: new(-13), fail: new(-12)})
		end := endPhase1(h, -10)
		assertPlan(t, h, journal.PhasesState{Phase: 2, Phase1End: end.Seq, RoundsLeft: 1, Candidates: []journal.CandidateState{
			{Core: 0, Offset: -10, SoloLimit: -13, Gap: 1},
		}})
	})
	t.Run("an open round counts as one and marks its movers", func(t *testing.T) {
		h := cleanCycleHarness(t, []int{-10, -10}, nil)
		nextRound(h)
		want := journal.PhasesState{Phase: 2, Phase1End: h.s.phases.phase1End, Round: 1, RoundsLeft: 1, Candidates: []journal.CandidateState{
			{Core: 0, Offset: -10, SoloLimit: -11, Gap: 1, Moving: true},
			{Core: 1, Offset: -10, SoloLimit: -11, Gap: 1, Moving: true},
		}}
		assertPlan(t, h, want)
		applyRound(h)
		assertPlan(t, h, want)
	})
	t.Run("a round that ended unpassed without blame carries its cores", func(t *testing.T) {
		h := cleanCycleHarness(t, []int{-10, -10}, nil)
		nextRound(h)
		applyRound(h)
		h.add(&journal.DeepeningRound{Round: 1, Event: journal.CycleEnd})
		assertPlan(t, h, journal.PhasesState{Phase: 2, Phase1End: h.s.phases.phase1End, RoundsLeft: 1, Candidates: []journal.CandidateState{
			{Core: 0, Offset: -11, SoloLimit: -11, Carried: true},
			{Core: 1, Offset: -11, SoloLimit: -11, Carried: true},
		}})
	})
	t.Run("a round re-checking carried cores in place does not move them", func(t *testing.T) {
		h := cleanCycleHarness(t, []int{-10, -10}, nil)
		nextRound(h)
		applyRound(h)
		h.add(&journal.DeepeningRound{Round: 1, Event: journal.CycleEnd, Reason: "a failure needs a hunt"}, h.s.round.seq)
		second := nextRound(h)
		assertPlan(t, h, journal.PhasesState{Phase: 2, Phase1End: h.s.phases.phase1End, Round: second.Round, RoundsLeft: 1, Candidates: []journal.CandidateState{
			{Core: 0, Offset: -11, SoloLimit: -11, Carried: true},
			{Core: 1, Offset: -11, SoloLimit: -11, Carried: true},
		}})
	})
	t.Run("a fresh mover shows the round's baseline", func(t *testing.T) {
		h := newHarness(t, soloCore(-10, -12), soloCore(-10, -12))
		h.add(&journal.ProfileChange{To: []int{-10, -10}})
		h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: h.s.steps})
		h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleEnd, Passed: true, Full: true})
		h.add(&journal.TunerDecision{Core: 0, Phase: journal.PhaseChecking, Decision: journal.Backoff, FromOffset: -10, ToOffset: -7, Pass: new(-12), FailurePoint: new(-13)})
		h.add(&journal.DeepeningRound{Round: 1, Event: journal.CycleStart, Profile: []int{-8, -11}, Target: []int{-12, -12}, Cores: []int{0, 1}, Trials: 1, TrialS: 30})
		plan := h.s.PhasePlan()
		got := map[int]int{}
		for _, c := range plan.Candidates {
			if c.Moving {
				got[c.Core] = c.Offset
			}
		}
		if diff := cmp.Diff(map[int]int{0: -7, 1: -10}, got); diff != "" {
			t.Fatalf("moving offsets (-want +got):\n%s", diff)
		}
		if diff := cmp.Diff(plan, replayState(h.events).PhasePlan()); diff != "" {
			t.Fatalf("phase plan after replay (-live +replayed):\n%s", diff)
		}
	})
	t.Run("the confirmation cycle leaves no rounds", func(t *testing.T) {
		h := newHarness(t, soloCore(-10, -13))
		end := endPhase1(h, -10)
		startCycle(h, 2)
		assertPlan(t, h, journal.PhasesState{Phase: 2, Phase1End: end.Seq, Confirming: true})
	})
	t.Run("after conclusion there is no phase", func(t *testing.T) {
		h := newHarness(t, soloCore(-10, -13))
		end := endPhase1(h, -10)
		startCycle(h, 2)
		conclusion := endCycle(h, 2)
		assertPlan(t, h, journal.PhasesState{Phase: 0, Phase1End: end.Seq, Concluded: conclusion.Seq})
	})
	t.Run("a reset returns to phase 1", func(t *testing.T) {
		h := newHarness(t, soloCore(-10, -13))
		endPhase1(h, -10)
		h.add(&journal.CorePhase{Core: 0, From: journal.PhaseHasRoom, To: journal.PhaseSearch, Offset: 0})
		assertPlan(t, h, journal.PhasesState{Phase: 1})
	})
	t.Run("no cores no plan", func(t *testing.T) {
		if got := replayState(nil).PhasePlan(); got != nil {
			t.Fatalf("plan %+v", got)
		}
	})
}

func TestProjectFillsPhasesAndBIOS(t *testing.T) {
	h := newHarness(t, soloCore(-10, -13), soloCore(-8, -9))
	st := projected(h)
	if st.Phases == nil || st.Phases.Phase != 1 || st.BIOS != nil {
		t.Fatalf("phases %+v, BIOS %+v before phase 1 ends", st.Phases, st.BIOS)
	}
	end := endPhase1(h, -10, -8)
	st = projected(h)
	want := &journal.BIOSState{Offsets: []int{-10, -8}, Confirmed: end.Seq}
	if diff := cmp.Diff(want, st.BIOS); diff != "" {
		t.Fatalf("BIOS (-want +got):\n%s", diff)
	}
	if !h.s.FirstResult() {
		t.Fatal("a confirmed profile is not the first result")
	}
}
