package tuner

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestRateBoundNeverUnderstates(t *testing.T) {
	for _, tc := range []struct {
		seconds int
		bound   float64
	}{{3600, 3}, {86400, .125}, {86600, .1248}, {7, 1542.8572}} {
		got := rateBound(tc.seconds)
		if got == nil || *got != tc.bound {
			t.Errorf("rateBound(%d) = %v, want %f", tc.seconds, got, tc.bound)
		}
	}
}

func TestTierClockResetsOnDeepFailure(t *testing.T) {
	h := residentHarness(t, -10)
	h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationStart, Steps: []machine.Regime{machine.R1}})
	h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationEnd, Clean: true, Qualifying: true})
	if h.s.QualifiedRotations() != 1 {
		t.Fatalf("qualified rotations = %d", h.s.QualifiedRotations())
	}
	prior := h.s.tierClockSeq
	k := machine.Workloads(machine.R1)[0].ID
	tr := Trial{Regime: machine.R1, Core: 0, Offset: -10, Condition: machine.Resident, Phase: journal.PhaseGuard, DurationS: 86400, Workload: k}
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: 86400})
	if total, _ := h.s.clean(); total != 86400 {
		t.Fatalf("clean %d, want 86400", total)
	}
	failure := h.add(&journal.Failure{Attribution: journal.Unattributed, Signal: machine.Crash, Condition: machine.Resident, Regime: machine.R6, Profile: []int{-10}})
	h.add(&journal.ProfileChange{From: []int{-10}, To: []int{-9}})
	if h.s.tierClockSeq != failure.Seq || h.s.tierClockSeq == prior {
		t.Fatalf("tier clock #%d, want failure #%d", h.s.tierClockSeq, failure.Seq)
	}
	if total, _ := h.s.clean(); total != 0 {
		t.Fatalf("clean since failure %d", total)
	}
	st := projected(h)
	if diff := cmp.Diff(failure.Seq, st.Guard.TierClockSeq); diff != "" {
		t.Fatalf("tier clock (-want +got):\n%s", diff)
	}
}

func TestFirstProfileStartsTierClock(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseDone, offset: -10})
	profile := h.add(&journal.ProfileChange{To: []int{-10}})
	st := projected(h)
	if st.Guard == nil {
		t.Fatal("no projected guard")
	}
	if diff := cmp.Diff(profile.Seq, st.Guard.TierClockSeq); diff != "" {
		t.Fatalf("first profile tier clock (-want +got):\n%s", diff)
	}
}
