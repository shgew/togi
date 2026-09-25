package tuner

import (
	"slices"
	"strings"
	"testing"

	"code.marleb.org/shgew/shycler/internal/config"
	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
)

// regainHarness has core 0 confirmed at -18 with failed mark -21 and two suspect counts, at its first guard trial.
func regainHarness(t *testing.T) *harness {
	t.Helper()
	h := newHarness(t, coreStart{phase: journal.PhaseConfirmed, offset: -18, pass: new(-18), fail: new(-21), unproven: 2})
	h.add(&journal.SessionBaseline{Offsets: []int{-5}})
	h.add(&journal.ConfigLoaded{Path: config.DefaultPath, Config: config.Default()})
	h.decideUntilTrial()
	return h
}

func (h *harness) expect(want ...string) {
	h.t.Helper()
	if got := h.until(); !slices.Equal(got, want) {
		h.t.Fatalf("got  %v\nwant %v", got, want)
	}
}

func lastPayload[P journal.Payload](h *harness) P {
	h.t.Helper()
	for i := len(h.events) - 1; i >= 0; i-- {
		if p, ok := h.events[i].Data.(P); ok {
			return p
		}
	}
	var zero P
	h.t.Fatalf("no %T", zero)
	return zero
}

func TestRegain(t *testing.T) {
	t.Parallel()
	t.Run("passing R1 to R5 regains one count", func(t *testing.T) {
		t.Parallel()
		h := regainHarness(t)
		h.add(&journal.CommandRegain{Cores: []int{0}})
		h.expect("confirmed->regain -19 u2", "end unclean", "trial R1 c0")
		for _, r := range machine.ConfirmationRegimes {
			a := h.s.Next()
			if a.Kind != RunTrial || a.Trial != (Trial{Core: 0, Offset: -19, Regime: r, Phase: journal.PhaseRegain, Condition: machine.Isolated}) {
				t.Fatalf("%s: %+v, want an isolated regain trial at -19", r, a)
			}
			h.trial(a, passed)
		}
		h.expect("regain->confirmed -19 u1", "profile", "start 2", "trial R1 c0")
		if p := lastPayload[*journal.CorePhase](h); p.Backoff {
			t.Fatalf("successful regain marked as backoff: %+v", p)
		}
		if p := lastPayload[*journal.ProfileChange](h); !slices.Equal(p.To, []int{-19}) {
			t.Fatalf("profile %v, want [-19]", p.To)
		}
		if c := projected(h).Cores[0]; *c.FailedMark != -21 || c.Queued != "" {
			t.Fatalf("core %+v", c)
		}
	})
	t.Run("a failed regain is a proven backoff to the confirmed offset", func(t *testing.T) {
		t.Parallel()
		h := regainHarness(t)
		h.add(&journal.CommandRegain{Cores: []int{0}})
		h.expect("confirmed->regain -19 u2", "end unclean", "trial R1 c0")
		h.trial(h.s.Next(), failed)
		h.expect("attributed 0 at -19", "regain->confirmed -18 u0", "profile", "start 2", "trial R1 c0")
		if p := lastPayload[*journal.CorePhase](h); !p.Backoff {
			t.Fatalf("failed regain not marked as backoff: %+v", p)
		}
		p := lastPayload[*journal.ProfileChange](h)
		if !slices.Equal(p.From, p.To) || !strings.Contains(p.Message(), "unchanged") {
			t.Fatalf("profile change %v -> %v: %q", p.From, p.To, p.Message())
		}
		if c := projected(h).Cores[0]; *c.FailedMark != -19 || c.UnprovenDepth != 0 {
			t.Fatalf("core %+v, want failed mark -19 and no unproven depth", c)
		}
	})
	t.Run("a proven backoff decided first cancels the queued regain", func(t *testing.T) {
		t.Parallel()
		h := regainHarness(t)
		h.end(h.intent(0, machine.R1), journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(0), DurationS: 30})
		h.add(&journal.CommandRegain{Cores: []int{0}})
		h.expect("attributed 0 at -18", "backoff 0 -18>-17 mark -18 u0", "end unclean", "profile", "start 2", "trial R1 c0")
		if c := projected(h).Cores[0]; c.Queued != "" {
			t.Fatalf("queued %q after the proven backoff", c.Queued)
		}
	})
}

func TestReset(t *testing.T) {
	t.Parallel()
	t.Run("search restarts from the baseline and the tier drops", func(t *testing.T) {
		t.Parallel()
		h := regainHarness(t)
		for range config.Default().Guard.Rotation {
			h.trial(h.s.Next(), passed)
		}
		h.expect("end clean", "tier bronze", "start 2", "trial R1 c0")
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
