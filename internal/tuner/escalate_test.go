package tuner

import (
	"slices"
	"strings"
	"testing"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type escalationLoad struct {
	workload int
	cores    []int
	top      int
}

// Loads are named by letter: A is the first R7 workload on CCD 0, B the second workload on CCD 0 and C the first
// workload on CCD 1. Each has unloaded cores off CO 0, so only the escalation rule decides whether it is located.
var escalationLoads = map[byte]escalationLoad{
	'a': {0, []int{0, 1}, 0},
	'b': {1, []int{0, 1}, 0},
	'c': {0, []int{2, 3}, 2},
}

func (l escalationLoad) trial(duration int) Trial {
	return Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[l.workload].ID, Cores: l.cores, DurationS: duration, Phase: journal.PhaseChecking, Condition: machine.Together}
}

// nextSkippingPhases decides the core phase changes that follow a backoff and returns the next other action.
func nextSkippingPhases(h *harness) Action {
	h.t.Helper()
	for {
		a := h.next()
		if _, ok := a.Payload.(*journal.CorePhase); !ok {
			return a
		}
		h.decide(a)
	}
}

// play runs ops against the harness: a lower-case letter passes that load, an upper-case letter fails it
// unattributed and, unless it is the last op, applies the backoff it requires. It returns the action that answers
// the last failure.
func play(h *harness, ops string) Action {
	h.t.Helper()
	for i := range len(ops) {
		load, fail := escalationLoads[ops[i]|0x20], ops[i] < 'a'
		if !fail {
			h.trial(Action{Kind: RunTrial, Trial: load.trial(120)}, passed)
			continue
		}
		end := failed
		end.DurationS, end.TopRequesters = 41, []int{load.top}
		h.trial(Action{Kind: RunTrial, Trial: load.trial(120)}, end)
		attribution := nextSkippingPhases(h)
		if f, ok := attribution.Payload.(*journal.Failure); !ok || f.Attribution != journal.Unattributed {
			h.t.Fatalf("attribution %+v", attribution)
		}
		h.decide(attribution)
		a := nextSkippingPhases(h)
		if i == len(ops)-1 {
			return a
		}
		move, ok := a.Payload.(*journal.TunerDecision)
		if !ok || move.Decision != journal.Backoff {
			h.t.Fatalf("op %d (%c) did not back off: %+v", i, ops[i], a)
		}
		h.decide(a)
		h.add(&journal.ProfileChange{To: h.s.offsets()})
	}
	return Action{}
}

func TestEscalationBoundaries(t *testing.T) {
	for _, tc := range []struct {
		name  string
		ops   string
		hunt  bool
		count string
	}{
		{"first failure of a load backs off", "A", false, "backoff 1 of this load"},
		{"one backoff is not enough", "AA", false, "backoff 2 of this load"},
		{"two backoffs escalate the next failure", "AAA", true, ""},
		{"a pass resets the count", "AAaA", false, "backoff 1 of this load"},
		{"a pass resets it again after two more backoffs", "AAaAA", false, "backoff 2 of this load"},
		{"two backoffs after a pass escalate", "AAaAAA", true, ""},
		{"another workload counts separately", "AAB", false, "backoff 1 of this load"},
		{"other loaded cores count separately", "AAC", false, "backoff 1 of this load"},
		{"alternating loads keep separate counts", "ABABA", true, ""},
		{"alternating loads stay below the count", "ABAB", false, "backoff 2 of this load"},
		{"a pass of another load does not reset", "AAbA", true, ""},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := r7Harness(t)
			a := play(h, tc.ops)
			if tc.hunt {
				start, ok := a.Payload.(*journal.HuntStart)
				if !ok || !slices.Equal(start.Candidates, []int{2, 3}) && !slices.Equal(start.Candidates, []int{0, 1}) {
					t.Fatalf("expected a located hunt, got %+v", a)
				}
				if missing := missingTokens(start.Reason, "backed off 2 times since its last passing trial", "still fails", "unloaded cores"); len(missing) > 0 {
					t.Fatalf("reason %q lacks %q", start.Reason, missing)
				}
				if len(a.Cause) != 1+escalateAfter {
					t.Fatalf("hunt start cause %v cites the failure and the %d backoffs", a.Cause, escalateAfter)
				}
				assertHuntNextReplay(h, a, (*State).Next, "replay lost the backoff count")
				return
			}
			move, ok := a.Payload.(*journal.TunerDecision)
			if !ok || move.Decision != journal.Backoff {
				t.Fatalf("expected a voltage-targeted backoff, got %+v", a)
			}
			if !strings.Contains(move.Reason, "voltage-targeted R7 backoff") || !strings.Contains(move.Reason, tc.count) || !strings.Contains(move.Reason, "located only after 2") {
				t.Fatalf("reason %q lacks %q", move.Reason, tc.count)
			}
			assertHuntNextReplay(h, a, (*State).Next, "replay lost the backoff count")
		})
	}
}

// A resumed session counts the same backoffs: the count is rebuilt from the journal events alone.
func TestEscalationCountSurvivesResume(t *testing.T) {
	h := r7Harness(t)
	for _, want := range []string{"backoff", "backoff", "hunt"} {
		ops := "A"
		if want == "backoff" && len(h.events) < 20 {
			ops = "AAaA"
		}
		a := play(h, ops)
		switch a.Payload.(type) {
		case *journal.TunerDecision:
			if want != "backoff" {
				t.Fatalf("resumed session backed off, want a located hunt: %+v", a)
			}
			h.decide(a)
			h.add(&journal.ProfileChange{To: h.s.offsets()})
			h.s = replayState(h.events)
		case *journal.HuntStart:
			if want != "hunt" {
				t.Fatalf("resumed session located early: %+v", a)
			}
		default:
			t.Fatalf("action %+v", a)
		}
	}
	assertProjectionReplay(h)
}

func TestEscalationWhenLoadedCoresCannotBackOff(t *testing.T) {
	t.Run("every loaded core of the affected CCD is at CO 0", func(t *testing.T) {
		h := r7Harness(t)
		for id := range 2 {
			h.add(&journal.CorePhase{Core: id, To: journal.PhaseHasRoom, Offset: 0, Reason: "test"})
		}
		h.add(&journal.ProfileChange{To: []int{0, 0, -30, -30}})
		a := play(h, "A")
		start, ok := a.Payload.(*journal.HuntStart)
		if !ok || len(a.Cause) != 1 {
			t.Fatalf("expected an immediate located hunt citing only the failure, got %+v", a)
		}
		if missing := missingTokens(start.Reason, "every loaded core", "CO 0", "cannot help"); len(missing) > 0 {
			t.Fatalf("reason %q lacks %q", start.Reason, missing)
		}
	})
	t.Run("a loaded core is still off CO 0", func(t *testing.T) {
		h := r7Harness(t)
		h.add(&journal.CorePhase{Core: 0, To: journal.PhaseHasRoom, Offset: 0, Reason: "test"})
		h.add(&journal.ProfileChange{To: []int{0, -30, -30, -30}})
		a := play(h, "A")
		move, ok := a.Payload.(*journal.TunerDecision)
		if !ok || move.Decision != journal.Backoff || move.Core != 1 {
			t.Fatalf("expected the step-down backoff of core 01, got %+v", a)
		}
	})
}

// A reset of a loaded core discards the load's failed trials, so the backoffs they caused no longer count: the next
// failure of the load is its first backoff again, live and on replay. A reset of a core outside the load keeps them.
func TestEscalationResetDropsDiscardedBackoffs(t *testing.T) {
	for _, tc := range []struct {
		name  string
		reset int
		hunt  bool
	}{
		{"reset of a loaded core restarts the count", 0, false},
		{"reset of a core outside the load keeps the count", 3, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := r7Harness(t)
			a := play(h, "AA")
			h.decide(a)
			h.add(&journal.ProfileChange{To: h.s.offsets()})
			offsets := h.s.offsets()
			h.add(&journal.CommandReset{Core: new(tc.reset)})
			for range 4 {
				reset := h.next()
				if _, ok := reset.Payload.(*journal.CorePhase); !ok {
					break
				}
				h.decide(reset)
			}
			// Put the reset core back where it was, so the next failure of the load is decided by the escalation rule alone.
			h.add(&journal.CorePhase{Core: tc.reset, To: journal.PhaseHasRoom, Offset: offsets[h.s.index(tc.reset)], Reason: "test"})
			h.add(&journal.ProfileChange{To: offsets})
			a = play(h, "A")
			if tc.hunt {
				if _, ok := a.Payload.(*journal.HuntStart); !ok {
					t.Fatalf("expected a located hunt, got %+v", a)
				}
				assertHuntNextReplay(h, a, (*State).Next, "replay lost the backoff count")
				return
			}
			move, ok := a.Payload.(*journal.TunerDecision)
			if !ok || move.Decision != journal.Backoff {
				t.Fatalf("expected a voltage-targeted backoff, got %+v", a)
			}
			if !strings.Contains(move.Reason, "backoff 1 of this load") {
				t.Fatalf("reason %q lacks backoff 1", move.Reason)
			}
			assertHuntNextReplay(h, a, (*State).Next, "replay kept the discarded backoffs")
		})
	}
}
