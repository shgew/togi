package tuner

import (
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/shycler/internal/config"
	"github.com/shgew/shycler/internal/journal"
	"github.com/shgew/shycler/internal/machine"
)

// confirmedHarness has core 0 confirmed at -18 with failed mark -21 and the given suspect counts, at its first guard
// trial.
func confirmedHarness(t *testing.T, unproven int) *harness {
	t.Helper()
	h := newHarness(t, coreStart{phase: journal.PhaseConfirmed, offset: -18, pass: new(-18), fail: new(-21), unproven: unproven})
	h.add(&journal.SessionBaseline{Offsets: []int{-5}})
	h.add(&journal.ConfigLoaded{Path: config.DefaultPath, Config: config.Default()})
	h.decideUntilTrial()
	return h
}

// suspectedHarness has four cores confirmed at -10, -12, -11 and -13 with failed marks three deeper, each backed off
// once on suspicion by an idle crash, at the first trial of rotation 2.
func suspectedHarness(t *testing.T) *harness {
	t.Helper()
	offsets := []int{-10, -12, -11, -13}
	marks := make([]*int, len(offsets))
	for i, o := range offsets {
		marks[i] = new(o - 3)
	}
	h, _ := newGuardHarness(t, offsets, marks)
	h.add(idleCrash())
	h.decideUntilTrial()
	return h
}

func idleCrash() *journal.Failure {
	return &journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Regime: machine.R6, Condition: machine.Resident}
}

// cleanEnd passes every trial and takes every decision until the clean rotation end, which it decides.
func (h *harness) cleanEnd() journal.Event {
	h.t.Helper()
	for range 1000 {
		a := h.s.Next()
		if a.Kind == RunTrial {
			h.trial(a, passed)
			continue
		}
		e := h.decide(a)
		if p, ok := a.Payload.(*journal.GuardRotation); ok && p.Event == journal.RotationEnd && p.Clean {
			return e
		}
	}
	h.t.Fatal("no clean rotation end after 1000 actions")
	return journal.Event{}
}

func (h *harness) expect(want ...string) {
	h.t.Helper()
	if got := h.until(); !slices.Equal(got, want) {
		h.t.Fatalf("got  %v\nwant %v", got, want)
	}
}

func lastPayload[P journal.Payload](h *harness) P {
	h.t.Helper()
	for _, e := range slices.Backward(h.events) {
		if p, ok := e.Data.(P); ok {
			return p
		}
	}
	var zero P
	h.t.Fatalf("no %T", zero)
	return zero
}

func TestAutomaticRegain(t *testing.T) {
	t.Parallel()
	h := suspectedHarness(t)
	end := h.cleanEnd()
	from := len(h.events)
	h.expect("regain 0 -9>-10 u0", "regain 2 -10>-11 u0", "regain 1 -11>-12 u0", "regain 3 -12>-13 u0", "profile", "start 3", "trial R2 c0")
	for _, e := range h.events[from:] {
		if p, ok := e.Data.(*journal.TunerDecision); ok && !slices.Equal(e.Cause, []int{end.Seq}) {
			t.Fatalf("regain of core %d cites %v, want the clean end %d", p.Core, e.Cause, end.Seq)
		}
	}
	if p := lastPayload[*journal.ProfileChange](h); !slices.Equal(p.To, []int{-10, -12, -11, -13}) {
		t.Fatalf("profile %v, want the confirmed offsets back", p.To)
	}
	if p := lastPayload[*journal.TunerDecision](h); !slices.Equal(p.SpentSteps, []int{-13}) || p.SettledMark != nil {
		t.Fatalf("last regain spent %v, settled %v; want [-13] and none", p.SpentSteps, p.SettledMark)
	}
}

func TestInterruptedRegain(t *testing.T) {
	t.Parallel()
	h := suspectedHarness(t)
	h.cleanEnd()
	h.decide(h.s.Next())
	resumed := New()
	for _, e := range h.events {
		resumed.Fold(e)
	}
	for _, core := range []int{2, 1, 3} {
		a := resumed.Next()
		if p, ok := a.Payload.(*journal.TunerDecision); !ok || p.Decision != journal.Regain || p.Core != core {
			t.Fatalf("after resuming: %s, want the regain of core %d", h.describe(a), core)
		}
		resumed.Fold(h.decide(a))
	}
	if a := resumed.Next(); h.describe(a) != "profile" {
		t.Fatalf("after every regain: %s, want profile", h.describe(a))
	}
}

func TestSpentStepSettles(t *testing.T) {
	t.Parallel()
	crash := journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash}
	h := suspectedHarness(t)
	h.cleanEnd()
	h.expect("regain 0 -9>-10 u0", "regain 2 -10>-11 u0", "regain 1 -11>-12 u0", "regain 3 -12>-13 u0", "profile", "start 3", "trial R2 c0")

	h.end(h.intent(0, machine.R1), crash)
	h.expect("unattributed", "suspect 0 -10>-9 u1", "end unclean", "profile", "start 4", "trial R2 c0")
	p := lastPayload[*journal.TunerDecision](h)
	if p.SettledMark == nil || *p.SettledMark != -10 || !strings.Contains(p.Reason, "the retry at -10 was spent, so the step is settled until reset --core") {
		t.Fatalf("suspect backoff from a spent step: settled %v, reason %q", p.SettledMark, p.Reason)
	}
	if c := projected(h).Cores[0]; c.UnprovenDepth != 1 || c.SettledDepth != 1 {
		t.Fatalf("core %+v, want unproven depth 1, all of it settled", c)
	}

	h.cleanEnd()
	h.expect("tier bronze", "start 5", "trial R2 c0")

	h.end(h.intent(0, machine.R1), journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(0), DurationS: 30})
	h.expect("attributed 0 at -9", "backoff 0 -9>-8 mark -9 u0", "end unclean", "tier none", "profile", "start 6", "trial R2 c0")
	p = lastPayload[*journal.TunerDecision](h)
	if p.SettledMark != nil || !slices.Equal(p.SpentSteps, []int{-10}) || p.UnprovenDepth != 0 {
		t.Fatalf("proven backoff: settled %v, spent %v, unproven %d; want none, [-10], 0", p.SettledMark, p.SpentSteps, p.UnprovenDepth)
	}

	h.add(&journal.CommandReset{Core: new(0)})
	h.decide(h.s.Next())
	if c := h.s.core(0); c.phase != journal.PhaseSearch || c.spent != nil || c.settled != nil {
		t.Fatalf("after reset: phase %s, spent %v, settled %v", c.phase, c.spent, c.settled)
	}
}

func TestFailureAfterCleanEndCancelsRegain(t *testing.T) {
	t.Parallel()
	h := suspectedHarness(t)
	h.cleanEnd()
	h.add(idleCrash())
	h.expect("suspect 0 -9>-8 u2", "suspect 2 -10>-9 u2", "suspect 1 -11>-10 u2", "suspect 3 -12>-11 u2", "profile", "start 3", "trial R2 c0")
}

func TestFailureAtZeroAfterCleanEndCancelsRegain(t *testing.T) {
	t.Parallel()
	h, _ := newGuardHarness(t, []int{-1}, []*int{new(-4)})
	h.add(idleCrash())
	h.decideUntilTrial()
	h.cleanEnd()
	h.add(idleCrash())
	if a := h.s.Next(); h.describe(a) != "dead end" {
		t.Fatalf("failure with every core at CO 0: %s, want the dead end", h.describe(a))
	}
	h.decide(h.s.Next())
	if p, ok := h.s.Next().Payload.(*journal.TunerDecision); ok && p.Decision == journal.Regain {
		t.Fatalf("after the dead end: regain of core %d, want none from the earlier clean end", p.Core)
	}
}

func TestReset(t *testing.T) {
	t.Parallel()
	t.Run("search restarts from the baseline and the tier drops", func(t *testing.T) {
		t.Parallel()
		h := confirmedHarness(t, 0)
		h.cleanEnd()
		h.expect("tier bronze", "start 2", "trial R2 c0")
		h.add(&journal.CommandReset{Core: new(0)})
		h.expect("confirmed->search -5 u0", "end unclean", "tier none", "trial R1 c0")
		if p := lastPayload[*journal.TierChange](h); p.Reason != "core 00 is in search" {
			t.Fatalf("tier reason %q", p.Reason)
		}
		c := projected(h).Cores[0]
		if c.FailedMark != nil || c.Pass != nil || c.UnprovenDepth != 0 || c.Phase != journal.PhaseSearch {
			t.Fatalf("core %+v, want a fresh search", c)
		}
	})
	t.Run("depth left to regain holds the tier back", func(t *testing.T) {
		t.Parallel()
		h := confirmedHarness(t, 2)
		h.cleanEnd()
		h.expect("regain 0 -18>-19 u1", "profile", "start 2", "trial R2 c0")
		if diff := cmp.Diff(journal.TierNone, projected(h).Tier); diff != "" {
			t.Fatalf("tier mismatch (-want +got):\n%s", diff)
		}
	})
	t.Run("reset clears a failed mark at 0", func(t *testing.T) {
		t.Parallel()
		h := newHarness(t, coreStart{phase: journal.PhaseConfirmed, offset: 0, fail: new(0)})
		h.add(&journal.SessionBaseline{Offsets: []int{-60}})
		if d, ok := h.s.Next().Payload.(*journal.DeadEnd); !ok || d.Condition != journal.DeadEndFailureAtZero {
			t.Fatal("no failure_at_zero before the reset")
		}
		h.add(&journal.CommandReset{Core: new(0)})
		h.expect("confirmed->search -50 u0", "trial R1 c0")
	})
	t.Run("reset drops a pending retry of the core", func(t *testing.T) {
		t.Parallel()
		h := newHarness(t, searchAt(-15)...)
		h.add(&journal.SessionBaseline{Offsets: []int{-5}})
		h.trial(h.s.Next(), unsure)
		h.add(&journal.CommandReset{Core: new(0)})
		h.expect("search->search -5 u0", "trial R1 c0")
		if a := h.s.Next(); a.Trial.Offset != -5 || a.Trial.Retry {
			t.Fatalf("trial %+v after the reset, want R1 at the baseline -5", a.Trial)
		}
	})
}
