package tuner

import (
	"slices"
	"testing"

	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
)

func TestTierFor(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name           string
		allConfirmed   bool
		regainable     bool
		dirty          bool
		cleanRotations int
		cleanS         int
		want           journal.Tier
	}{
		{"a core unconfirmed", false, false, false, 5, 400000, journal.TierNone},
		{"profile changed", true, false, true, 5, 400000, journal.TierNone},
		{"depth left to regain", true, true, false, 5, 400000, journal.TierNone},
		{"no clean rotation", true, false, false, 0, 400000, journal.TierNone},
		{"one rotation, no hours", true, false, false, 1, 0, journal.TierBronze},
		{"just under 24 h", true, false, false, 1, 86399, journal.TierBronze},
		{"24 h", true, false, false, 3, 86400, journal.TierSilver},
		{"just under 100 h", true, false, false, 12, 359999, journal.TierSilver},
		{"100 h", true, false, false, 13, 360000, journal.TierGold},
		{"1000 h is still gold", true, false, false, 125, 3600000, journal.TierGold},
	}
	for _, tt := range tests {
		if got := tierFor(tt.allConfirmed, tt.regainable, tt.dirty, tt.cleanRotations, tt.cleanS); got != tt.want {
			t.Errorf("%s: %s, want %s", tt.name, got, tt.want)
		}
	}
}

func TestRateBoundNeverUnderstates(t *testing.T) {
	t.Parallel()
	if got := rateBound(0); got != nil {
		t.Errorf("rateBound(0) = %v, want nil", *got)
	}
	for _, tt := range []struct {
		cleanS int
		want   float64
	}{
		{3600, 3},
		{86400, 0.125},
		{86600, 0.1248},
		{7, 1542.8572},
	} {
		got := rateBound(tt.cleanS)
		if got == nil || *got != tt.want || *got < 10800/float64(tt.cleanS) {
			t.Errorf("rateBound(%d) = %v, want %v", tt.cleanS, got, tt.want)
		}
	}
}

func TestTierTransitions(t *testing.T) {
	t.Parallel()
	h, a := newGuardHarness(t, []int{-10}, []*int{new(-11)})
	type change struct {
		to     journal.Tier
		hours  int
		cause  journal.Kind
		reason string
	}
	var changes []change
	hours := 0
	runUntil := func(target int) {
		t.Helper()
		for range 1000 {
			if a.Kind == RunTrial {
				if hours == target {
					return
				}
				h.trial(a, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: 3600})
				hours++
			} else {
				e := h.decide(a)
				if p, ok := e.Data.(*journal.TierChange); ok {
					changes = append(changes, change{p.To, hours, h.events[e.Cause[0]-1].Kind, p.Reason})
				}
			}
			a = h.s.Next()
		}
		t.Fatalf("no trial slot at %d h", target)
	}
	runUntil(100)
	want := []change{
		{journal.TierBronze, 8, journal.KindGuardRotation, "every core is confirmed, nothing is left to regain and the profile survived a clean rotation"},
		{journal.TierSilver, 24, journal.KindTrialEnd, "24 clean hours since the profile change"},
		{journal.TierGold, 100, journal.KindTrialEnd, "100 clean hours since the profile change"},
	}
	if !slices.Equal(changes, want) {
		t.Fatalf("changes %+v, want %+v", changes, want)
	}
	if st := projected(h); st.Tier != journal.TierGold || st.TierSeq == 0 || h.events[st.TierSeq-1].Kind != journal.KindTierChange {
		t.Fatalf("projected tier %s at #%d", st.Tier, st.TierSeq)
	}

	h.end(h.intent(0, machine.R2), journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(0), DurationS: 30})
	got := h.until()
	if w := []string{"attributed 0 at -10", "backoff 0 -10>-9 mark -10 u0", "end unclean", "tier none", "profile", "start 14", "trial R1 c0"}; !slices.Equal(got, w) {
		t.Fatalf("after a failure: %v, want %v", got, w)
	}
	if last := h.events[len(h.events)-3].Data.(*journal.TierChange); last.Reason != "the profile changed" {
		t.Fatalf("drop reason %q", last.Reason)
	}

	changes, hours, a = nil, 0, h.s.Next()
	runUntil(8)
	if want := []change{{journal.TierBronze, 8, journal.KindGuardRotation, "every core is confirmed, nothing is left to regain and the profile survived a clean rotation"}}; !slices.Equal(changes, want) {
		t.Fatalf("after the new rotation: %+v, want %+v", changes, want)
	}
}
