package tuner

import (
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func soloCore(offset, solo int) coreStart {
	c := coreStart{from: journal.PhaseSearch, phase: journal.PhaseHasRoom, offset: offset, pass: new(solo)}
	if solo > machine.MinOffset {
		c.fail = new(solo - 1)
	}
	return c
}

// drive advances h with passing trials and the tuner's own decisions until done holds, and requires every step to
// resume identically from a fresh fold of the journal so far.
func drive(t *testing.T, h *harness, done func() bool) {
	t.Helper()
	for range 4000 {
		if done() {
			return
		}
		if diff := cmp.Diff(h.s.Next(), replayState(h.events).Next()); diff != "" {
			t.Fatalf("resume after event %d (-live +replayed):\n%s", len(h.events), diff)
		}
		if live, replayed := h.s.phases, replayState(h.events).phases; live.phase1End != replayed.phase1End || live.confirmStart != replayed.confirmStart || live.concluded != replayed.concluded || !slices.Equal(live.passed, replayed.passed) || !slices.Equal(live.confirmed, replayed.confirmed) {
			t.Fatalf("phase state after event %d: live %+v, replayed %+v", len(h.events), live, replayed)
		}
		a := h.next()
		if a.Kind == RunTrial {
			h.trial(a, passed)
			continue
		}
		h.decide(a)
	}
	t.Fatal("never reached the awaited state")
}

func kinds(h *harness, from int) []string {
	var out []string
	for _, e := range h.events[from:] {
		switch p := e.Data.(type) {
		case *journal.DeepeningRound:
			out = append(out, "round-"+string(p.Event))
		case *journal.CheckingCycle:
			out = append(out, "cycle-"+string(p.Event))
		}
	}
	return out
}

func TestSearchExitStartsOneCountShallowerThanTheSoloLimit(t *testing.T) {
	for _, tt := range []struct {
		name       string
		solo       int
		fail       *int
		start      int
		wantPhase  journal.Phase
		wantReason string
	}{
		{"interior", -20, new(-21), -19, journal.PhaseHasRoom, "one count shallower than the solo limit -20 (margin)"},
		{"floor", -50, nil, -49, journal.PhaseHasRoom, "checking starts at -49"},
		{"zero", 0, nil, 0, journal.PhaseHasRoom, "checking starts at 0"},
		{"failure point two counts deeper", -20, new(-22), -19, journal.PhaseHasRoom, "margin"},
	} {
		t.Run(tt.name, func(t *testing.T) {
			h := newHarness(t, coreStart{phase: journal.PhaseSearch, offset: tt.solo, fail: tt.fail, check: true})
			var got *journal.CorePhase
			for range 200 {
				a := h.next()
				if a.Kind == Decide {
					p, ok := h.decide(a).Data.(*journal.CorePhase)
					if ok && p.To != journal.PhaseSearch {
						got = p
						break
					}
					continue
				}
				h.trial(a, passed)
			}
			if got == nil {
				t.Fatal("core never left search")
			}
			if got.Offset != tt.start || got.Pass == nil || *got.Pass != tt.solo || got.To != tt.wantPhase || !strings.Contains(got.Reason, tt.wantReason) {
				t.Fatalf("search exit %+v, want checking at %d with solo limit %d: %q", got, tt.start, tt.solo, tt.wantReason)
			}
			if c := h.s.core(0); !c.hasSoloLimit || c.soloLimit != tt.solo {
				t.Fatalf("solo limit %d (%t), want %d", c.soloLimit, c.hasSoloLimit, tt.solo)
			}
		})
	}
}

func TestSoloLimitSurvivesAFailureDiscardingPass(t *testing.T) {
	h := newHarness(t, soloCore(-19, -20))
	h.add(&journal.CorePhase{Core: 0, From: journal.PhaseSearch, To: journal.PhaseHasRoom, Offset: -19, Pass: new(-20), FailurePoint: new(-21)})
	h.add(&journal.TunerDecision{Core: 0, Phase: journal.PhaseChecking, Decision: journal.Backoff, FromOffset: -19, ToOffset: -18, FailurePoint: new(-19)})
	if c := h.s.core(0); c.pass != nil || !c.hasSoloLimit || c.soloLimit != -20 {
		t.Fatalf("pass %v solo limit %d (%t), want the solo limit kept", c.pass, c.soloLimit, c.hasSoloLimit)
	}
	h.add(&journal.CorePhase{Core: 0, From: journal.PhaseHasRoom, To: journal.PhaseSearch, Offset: 0})
	if h.s.core(0).hasSoloLimit {
		t.Fatal("reset kept the solo limit")
	}
}

func TestPhase1NeverDeepensAndEndsAtItsFirstPassedFullCycle(t *testing.T) {
	h := newHarness(t, soloCore(-10, -11), soloCore(-10, -11))
	h.add(&journal.ProfileChange{To: []int{-10, -10}})
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{machine.R1}})
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleEnd, Passed: true})
	if h.s.phases.phase1End != 0 || h.s.phase2RoundDue() {
		t.Fatalf("a passed cycle that is not full ended phase 1: %+v", h.s.phases)
	}
	h.add(&journal.CheckingCycle{Cycle: 2, Event: journal.CycleStart, Steps: []machine.Regime{machine.R1}})
	for range 3 {
		a := h.next()
		if p, ok := a.Payload.(*journal.DeepeningRound); ok {
			t.Fatalf("phase 1 deepened: %+v", p)
		}
		if a.Kind == RunTrial {
			h.trial(a, passed)
		}
	}
	h.add(&journal.CheckingCycle{Cycle: 2, Event: journal.CycleEnd, Passed: true, Full: true})
	if h.s.phases.phase1End == 0 || !slices.Equal(h.s.phases.confirmed, []int{-10, -10}) {
		t.Fatalf("phase 1 did not end at its first passed full cycle: %+v", h.s.phases)
	}
	if !h.s.phase2RoundDue() {
		t.Fatal("no round is due although every core can move")
	}
}

func TestPhase2Moves(t *testing.T) {
	combination := func(members ...journal.CombinationMember) *journal.Combination {
		return &journal.Combination{Combination: 1, Hunt: 1, Members: members}
	}
	for _, tt := range []struct {
		name      string
		starts    []coreStart
		extra     []journal.Payload
		wantMoved []int
		want      []int
	}{
		{"one count each toward the solo limit", []coreStart{soloCore(-10, -13), soloCore(-8, -9)}, nil, []int{0, 1}, []int{-11, -9}},
		{"a core at its solo limit stays", []coreStart{soloCore(-10, -10), soloCore(-8, -9)}, nil, []int{1}, []int{-10, -9}},
		{"a failure point at the next count blocks", []coreStart{{from: journal.PhaseSearch, phase: journal.PhaseHasRoom, offset: -10, pass: new(-12), fail: new(-11)}, soloCore(-8, -9)}, nil, []int{1}, []int{-10, -9}},
		{"a combination blocks only the second member", []coreStart{soloCore(-10, -12), soloCore(-10, -12)}, []journal.Payload{combination(journal.CombinationMember{Core: 0, Offset: -11}, journal.CombinationMember{Core: 1, Offset: -11})}, []int{0}, []int{-11, -10}},
		{"a core with no recorded solo limit is no candidate", []coreStart{{phase: journal.PhaseHasRoom, offset: -10, pass: new(-12)}}, nil, nil, []int{-10}},
		{"a multi-count voltage-targeted gap moves down to its failure point plus one", []coreStart{{from: journal.PhaseSearch, phase: journal.PhaseHasRoom, offset: -5, pass: new(-12), fail: new(-13)}}, nil, []int{0}, []int{-6}},
		{"solo limit zero is no candidate", []coreStart{soloCore(0, 0)}, nil, nil, []int{0}},
	} {
		t.Run(tt.name, func(t *testing.T) {
			h := newHarness(t, tt.starts...)
			for _, p := range tt.extra {
				h.add(p)
			}
			got, moved, carried := h.s.phase2Moves()
			if diff := cmp.Diff(tt.want, got); diff != "" {
				t.Fatalf("profile (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff(tt.wantMoved, moved); diff != "" || len(carried) > 0 {
				t.Fatalf("moved (-want +got):\n%s carried %v", diff, carried)
			}
		})
	}
}

func TestPhase2RoundMovesEveryCandidateOneCountAndResumes(t *testing.T) {
	h := cleanCycleHarness(t, []int{-10, -10, -10, -10}, nil)
	from := len(h.events)
	drive(t, h, func() bool { return len(kinds(h, from)) > 0 && h.s.round != nil })
	r := h.s.round.start
	if r.Round != 1 || !slices.Equal(r.Cores, []int{0, 1, 2, 3}) || !slices.Equal(r.Profile, []int{-11, -11, -11, -11}) || !slices.Equal(r.Target, []int{-11, -11, -11, -11}) {
		t.Fatalf("round %+v", r)
	}
	if r.BaseSeq != h.s.passedFullCycles[0].seq {
		t.Fatalf("round base #%d, want the phase-1 cycle #%d", r.BaseSeq, h.s.passedFullCycles[0].seq)
	}
}

func TestPhase2RunsRoundsThenAlwaysTheConfirmationCycle(t *testing.T) {
	h := cleanCycleHarness(t, []int{-10, -10}, nil)
	from := len(h.events)
	drive(t, h, func() bool { return h.s.CleanCycles() == 1 })
	got := kinds(h, from)
	want := []string{"round-start", "round-end", "cycle-start", "cycle-end"}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("phase 2 events (-want +got):\n%s", diff)
	}
	if !slices.Equal(h.s.Profile(), []int{-11, -11}) || !slices.Equal(h.s.phases.passed, []int{-11, -11}) {
		t.Fatalf("confirmed profile %v, passed %v", h.s.Profile(), h.s.phases.passed)
	}
	for _, e := range h.events[from:] {
		if p, ok := e.Data.(*journal.CheckingCycle); ok && p.Event == journal.CycleStart && !strings.Contains(p.Reason, "confirmation") {
			t.Fatalf("confirmation cycle start reason %q", p.Reason)
		}
	}
}

func TestZeroMovableCandidatesStillRunTheFullConfirmationCycle(t *testing.T) {
	h := newHarness(t, soloCore(-10, -10), soloCore(-12, -12))
	var phase1End int
	drive(t, h, func() bool {
		if phase1End == 0 && h.s.phases.phase1End != 0 {
			phase1End = len(h.events)
		}
		return h.s.CleanCycles() == 1
	})
	if phase1End == 0 {
		t.Fatal("phase 1 never ended")
	}
	end := h.events[phase1End-1].Data.(*journal.CheckingCycle)
	if !end.Passed || !end.Full || !strings.Contains(end.Reason, "no core can move") {
		t.Fatalf("phase 1 end %+v", end)
	}
	after := kinds(h, phase1End)
	if diff := cmp.Diff([]string{"cycle-start", "cycle-end"}, after); diff != "" {
		t.Fatalf("events after phase 1 (-want +got):\n%s", diff)
	}
	var confirmation *journal.CheckingCycle
	for _, e := range h.events[phase1End:] {
		if p, ok := e.Data.(*journal.CheckingCycle); ok && p.Event == journal.CycleStart {
			confirmation = p
		}
	}
	if confirmation == nil || !strings.Contains(confirmation.Reason, "confirmation") {
		t.Fatalf("confirmation start %+v", confirmation)
	}
	if h.s.phases.concluded <= h.s.phases.phase1End {
		t.Fatalf("phase 2 concluded at phase 1's cycle: %+v", h.s.phases)
	}
}

func TestZeroMovableCandidatesNeverConcludeAtPhase1End(t *testing.T) {
	h := newHarness(t, soloCore(-10, -10))
	drive(t, h, func() bool { return h.s.phases.phase1End != 0 })
	if h.s.CleanCycles() != 0 {
		t.Fatalf("phase 1's cycle counted as clean")
	}
	a := h.next()
	g, ok := a.Payload.(*journal.CheckingCycle)
	if h.s.phase2RoundDue() || !ok && a.Kind != Decide || ok && (g.Event != journal.CycleStart || !strings.Contains(g.Reason, "confirmation")) {
		t.Fatalf("next after phase 1 with no candidate: %+v", a)
	}
}

func roundFailure(t *testing.T, h *harness, core int) journal.Event {
	t.Helper()
	a := h.next()
	for a.Kind != RunTrial || a.Trial.Round == 0 || a.Trial.Condition != machine.Alone || a.Trial.Core != core || a.Trial.Regime != machine.R1 {
		if a.Kind == RunTrial {
			h.trial(a, passed)
		} else {
			h.decide(a)
		}
		a = h.next()
	}
	h.trial(a, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(core)})
	return h.decide(h.next())
}

func TestRoundFailureBlamesTheLoadedDeepenedCoreAndRollsBackOnlyLoadedMovers(t *testing.T) {
	h := cleanCycleHarness(t, []int{-10, -10, -10, -10}, nil)
	drive(t, h, func() bool { return h.s.round != nil && len(h.s.round.start.Cores) == 4 })
	for {
		a, ok := h.s.roundMoves()
		if !ok {
			break
		}
		h.decide(a)
	}
	failure := roundFailure(t, h, 0)
	a := h.next()
	end, ok := a.Payload.(*journal.DeepeningRound)
	if !ok || end.Event != journal.CycleEnd || end.Passed || !slices.Equal(a.Cause, []int{failure.Seq}) {
		t.Fatalf("round end %+v", a)
	}
	h.decide(a)
	a = h.next()
	back, ok := a.Payload.(*journal.TunerDecision)
	if !ok || back.Phase != journal.PhaseDeepening || back.Decision != journal.Backoff || back.Core != 0 || back.ToOffset != -10 || back.FailurePoint == nil || *back.FailurePoint != -11 || !slices.Equal(a.Cause, []int{failure.Seq}) {
		t.Fatalf("blamed backoff %+v", a)
	}
	h.decide(a)
	if got := h.s.offsets(); !slices.Equal(got, []int{-10, -11, -11, -11}) {
		t.Fatalf("unloaded movers did not keep their offset: %v", got)
	}
	for _, c := range h.s.cores {
		if c.pending != 0 {
			t.Fatalf("core %d still pending failure #%d", c.id, c.pending)
		}
	}
	for range 40 {
		a = h.next()
		if p, ok := a.Payload.(*journal.DeepeningRound); ok && p.Event == journal.CycleStart {
			if !slices.Equal(p.Cores, []int{1, 2, 3}) || !slices.Equal(p.Profile, []int{-10, -11, -11, -11}) {
				t.Fatalf("second round %+v", p)
			}
			return
		}
		if _, ok := a.Payload.(*journal.HuntStart); ok {
			t.Fatalf("the consumed failure started a hunt: %+v", a)
		}
		if a.Kind == RunTrial {
			h.trial(a, passed)
		} else {
			h.decide(a)
		}
	}
	t.Fatal("no second round")
}

func TestRoundFailureMovesNothingIntoARecordedFailurePoint(t *testing.T) {
	h := cleanCycleHarness(t, []int{-10, -10}, nil)
	drive(t, h, func() bool { return h.s.round != nil })
	for {
		a, ok := h.s.roundMoves()
		if !ok {
			break
		}
		h.decide(a)
	}
	roundFailure(t, h, 0)
	for range 40 {
		a := h.next()
		if a.Kind == RunTrial {
			h.trial(a, passed)
		} else {
			h.decide(a)
		}
		if h.s.phases.confirmStart != 0 {
			break
		}
	}
	if c := h.s.core(0); c.offset != -10 || c.fail == nil || *c.fail != -11 {
		t.Fatalf("blamed core ends at %d with failure point %v", c.offset, c.fail)
	}
	for _, e := range h.events {
		if p, ok := e.Data.(*journal.DeepeningRound); ok && p.Event == journal.CycleStart && p.Round > 1 && slices.Contains(p.Cores, 0) && p.Profile[0] < -10 {
			t.Fatalf("round %d retried core 0's failure point: %+v", p.Round, p)
		}
	}
}

func TestConfirmationFailureRevertsDeepenedCoresTopRequesterFirst(t *testing.T) {
	h := cleanCycleHarness(t, []int{-10, -10, -10, -10}, nil)
	drive(t, h, func() bool { return h.s.phases.confirmStart != 0 && h.s.checking.open })
	idle := func(profile []int) journal.Event {
		return h.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Regime: machine.R6, Condition: machine.Together, Profile: profile})
	}
	revert := func(failure journal.Event, core, to int) {
		t.Helper()
		a := h.next()
		d, ok := a.Payload.(*journal.TunerDecision)
		if !ok || d.Phase != journal.PhaseDeepening || d.Decision != journal.Backoff || d.Core != core || d.ToOffset != to || d.FailurePoint == nil || *d.FailurePoint != -11 || !slices.Equal(a.Cause, []int{failure.Seq}) {
			t.Fatalf("revert %+v, want core %d back to %d", a, core, to)
		}
		h.decide(a)
	}
	first := idle([]int{-11, -11, -11, -11})
	revert(first, 3, -10)
	second := idle([]int{-11, -11, -11, -10})
	revert(second, 2, -10)
	third := idle([]int{-11, -11, -10, -10})
	revert(third, 1, -10)
	fourth := idle([]int{-11, -10, -10, -10})
	revert(fourth, 0, -10)
	last := idle([]int{-10, -10, -10, -10})
	a := h.next()
	if d, ok := a.Payload.(*journal.TunerDecision); ok && d.Phase == journal.PhaseDeepening {
		t.Fatalf("a failure with no deepened core loaded was blamed on a deepened core: %+v", a)
	}
	if len(h.s.queue) == 0 || h.s.queue[0].seq != last.Seq {
		t.Fatalf("failure #%d with every deepened core reverted did not follow the ordinary rules", last.Seq)
	}
	assertProjectionReplay(h)
}

func TestConfirmationResumesAcrossRevertAndRerun(t *testing.T) {
	h := cleanCycleHarness(t, []int{-10, -10}, nil)
	drive(t, h, func() bool { return h.s.phases.confirmStart != 0 && h.s.checking.open })
	h.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Regime: machine.R6, Condition: machine.Together, Profile: []int{-11, -11}})
	reverted := false
	drive(t, h, func() bool {
		if d, ok := h.events[len(h.events)-1].Data.(*journal.TunerDecision); ok && d.Phase == journal.PhaseDeepening {
			reverted = true
		}
		return reverted && h.s.CleanCycles() == 1
	})
	if h.s.phases.concluded == 0 || len(h.s.obligations) != 0 {
		t.Fatalf("confirmation did not conclude after its revert: %+v", h.s.phases)
	}
}

func TestPhase2BlameInDrainMatchesNext(t *testing.T) {
	h := cleanCycleHarness(t, []int{-10, -10}, nil)
	drive(t, h, func() bool { return h.s.phases.confirmStart != 0 && h.s.checking.open })
	h.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Regime: machine.R6, Condition: machine.Together, Profile: []int{-11, -11}})
	drained, ok := h.s.Drain()
	if !ok || cmp.Diff(drained, h.s.Next()) != "" {
		t.Fatalf("drain %+v (%t), next %+v", drained, ok, h.s.Next())
	}
}

func TestRoundFailureBlamesTopRequestGroupOfAnR7Load(t *testing.T) {
	h := cleanCycleHarness(t, []int{-10, -10, -10, -10}, nil)
	drive(t, h, func() bool { return h.s.round != nil })
	for {
		a, ok := h.s.roundMoves()
		if !ok {
			break
		}
		h.decide(a)
	}
	var r7 Action
	for range 400 {
		a := h.next()
		if a.Kind == RunTrial && a.Trial.Regime == machine.R7 && a.Trial.Round != 0 {
			r7 = a
			break
		}
		if a.Kind == RunTrial {
			h.trial(a, passed)
		} else {
			h.decide(a)
		}
	}
	if r7.Kind != RunTrial {
		t.Fatal("the round never ran an R7 check")
	}
	loaded := slices.Clone(r7.Trial.Cores)
	h.trial(r7, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash})
	failure := h.decide(h.next())
	end := h.next()
	if p, ok := end.Payload.(*journal.DeepeningRound); !ok || p.Event != journal.CycleEnd || !slices.Equal(end.Cause, []int{failure.Seq}) {
		t.Fatalf("round end %+v", end)
	}
	h.decide(end)
	var blamed *journal.TunerDecision
	for range 6 {
		a := h.next()
		d, ok := a.Payload.(*journal.TunerDecision)
		if !ok || d.Phase != journal.PhaseDeepening {
			t.Fatalf("action %+v", a)
		}
		h.decide(a)
		if d.Decision == journal.Backoff {
			blamed = d
			break
		}
		if !slices.Contains(loaded, d.Core) {
			t.Fatalf("rollback yielded unloaded core %d (loaded %v)", d.Core, loaded)
		}
	}
	if blamed == nil || !slices.Contains(loaded, blamed.Core) {
		t.Fatalf("blamed %+v outside the loaded cores %v", blamed, loaded)
	}
	if want := loaded[len(loaded)-1]; blamed.Core != want {
		t.Fatalf("blamed core %d, want the least preferred loaded core %d", blamed.Core, want)
	}
	if len(h.s.loads) != 0 {
		t.Fatalf("a phase-2 blame counted toward escalation: %v", h.s.loads)
	}
}
