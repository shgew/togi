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
	if total, _, _, _ := h.s.clean(); total != 86400 {
		t.Fatalf("clean %d, want 86400", total)
	}
	failure := h.add(&journal.Failure{Attribution: journal.Unattributed, Signal: machine.Crash, Condition: machine.Resident, Regime: machine.R6, Profile: []int{-10}})
	h.add(&journal.ProfileChange{From: []int{-10}, To: []int{-9}})
	if h.s.tierClockSeq != failure.Seq || h.s.tierClockSeq == prior {
		t.Fatalf("tier clock #%d, want failure #%d", h.s.tierClockSeq, failure.Seq)
	}
	if total, _, _, _ := h.s.clean(); total != 0 {
		t.Fatalf("clean since failure %d", total)
	}
	st := projected(h)
	if diff := cmp.Diff(failure.Seq, st.Guard.TierClockSeq); diff != "" {
		t.Fatalf("tier clock (-want +got):\n%s", diff)
	}
}

func TestEarlierRotationQualifiesAnUncontradictedShallowerOrEqualProfile(t *testing.T) {
	failure := func(profile []int) journal.Payload {
		return &journal.Failure{Attribution: journal.Unattributed, Signal: machine.Crash, Condition: machine.Resident, Regime: machine.R6, Profile: profile}
	}
	for _, tt := range []struct {
		name  string
		after []journal.Payload
		final []int
		want  int
	}{
		{"returns to the qualified profile", nil, []int{-10, -12}, 1},
		{"shallower than the qualified profile", nil, []int{-10, -11}, 1},
		{"deeper than the qualified profile", nil, []int{-11, -12}, 0},
		{"failure at the qualified profile", []journal.Payload{failure([]int{-10, -12})}, []int{-10, -12}, 0},
		{"failure at a deeper profile", []journal.Payload{failure([]int{-11, -13})}, []int{-10, -12}, 1},
		{"incomplete failure profile", []journal.Payload{failure([]int{-10})}, []int{-10, -12}, 0},
		{"failure at an incomparable profile", []journal.Payload{failure([]int{-11, -11})}, []int{-10, -12}, 1},
		{"failure at a profile as shallow as the qualified one", []journal.Payload{failure([]int{-9, -12})}, []int{-10, -12}, 0},
		{"reset after qualification", []journal.Payload{&journal.CommandReset{Core: new(1)}}, []int{-10, -12}, 0},
	} {
		t.Run(tt.name, func(t *testing.T) {
			h := residentHarness(t, -10, -12)
			h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationStart, Steps: []machine.Regime{machine.R1}})
			rotation := h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationEnd, Clean: true, Qualifying: true})
			h.add(&journal.ProfileChange{From: []int{-10, -12}, To: []int{-11, -11}})
			for _, p := range tt.after {
				h.add(p)
			}
			h.add(&journal.ProfileChange{From: []int{-11, -11}, To: tt.final})
			if got := h.s.QualifiedRotations(); got != tt.want {
				t.Fatalf("qualified rotations = %d, want %d", got, tt.want)
			}
			if tt.want == 1 && h.s.creditedRotation() != rotation.Seq {
				t.Fatalf("credited rotation #%d, want #%d", h.s.creditedRotation(), rotation.Seq)
			}
		})
	}
}

func TestCreditedRotationKeepsTheTierClockAndReplays(t *testing.T) {
	h := residentHarness(t, -10, -12)
	h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationStart, Steps: []machine.Regime{machine.R1}})
	rotation := h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationEnd, Clean: true, Qualifying: true})
	tr := Trial{Regime: machine.R1, Core: 0, Offset: -10, Condition: machine.Resident, Phase: journal.PhaseGuard, DurationS: goldCleanS}
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: goldCleanS})
	deepening := h.add(&journal.ProfileChange{From: []int{-10, -12}, To: []int{-11, -12}})
	profile := h.add(&journal.ProfileChange{From: []int{-11, -12}, To: []int{-10, -12}})

	action, ok := h.s.tierNext()
	if !ok {
		t.Fatal("restored profile did not earn a tier")
	}
	if tier := action.Payload.(*journal.TierChange).To; tier != journal.TierBronze {
		t.Fatalf("credited rotation earned %s, want Bronze without old clean hours", tier)
	}
	if diff := cmp.Diff([]int{rotation.Seq, profile.Seq}, action.Cause); diff != "" {
		t.Fatalf("credited rotation cause (-want +got):\n%s", diff)
	}
	state := projected(h)
	if state.Guard.TierClockSeq != deepening.Seq || state.Guard.CleanS != 0 || state.Guard.CleanRotations != 1 {
		t.Fatalf("credited rotation changed exposure: %+v", state.Guard)
	}
	replayed := New()
	for _, event := range h.events {
		replayed.Fold(event)
	}
	got, ok := replayed.tierNext()
	if !ok {
		t.Fatal("replay lost the credited rotation")
	}
	if diff := cmp.Diff(action, got); diff != "" {
		t.Fatalf("credited tier after replay (-want +got):\n%s", diff)
	}
}

func TestEarlierRotationRequiresDoneCoresAtItsEnd(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseResident, offset: -10})
	h.decide(h.next())
	h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationStart, Steps: []machine.Regime{machine.R1}})
	h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationEnd, Clean: true, Qualifying: true})
	h.add(&journal.CorePhase{Core: 0, To: journal.PhaseDone, Offset: -10, FailedMark: new(-11)})
	h.add(&journal.ProfileChange{From: []int{-10}, To: []int{-11}})
	h.add(&journal.ProfileChange{From: []int{-11}, To: []int{-10}})
	if got := h.s.QualifiedRotations(); got != 0 {
		t.Fatalf("rotation that ended before every core was done counted: %d", got)
	}
	action, ok := h.s.tierNext()
	if ok && action.Payload.(*journal.TierChange).To != journal.TierNone {
		t.Fatalf("rotation that ended before every core was done earned a tier: %+v", action)
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

func TestTemperatureFollowsCleanExposure(t *testing.T) {
	h := residentHarness(t, -10, -12)
	tr := Trial{Regime: machine.R6, Cores: []int{0, 1}, Condition: machine.Resident, Phase: journal.PhaseGuard, DurationS: 120}
	_, peak := h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: 120, TctlMaxC: new(92)})
	before := projected(h).Guard
	h.add(&journal.ProfileChange{From: []int{-10, -12}, To: []int{-9, -12}})
	after := projected(h).Guard
	if after.CleanS != before.CleanS || after.TierClockSeq != before.TierClockSeq || after.TctlMaxC == nil || *after.TctlMaxC != 92 || after.TctlMaxSeq != peak.Seq {
		t.Fatalf("shallow change lost counted peak: before %+v, after %+v", before, after)
	}
	failure := h.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Condition: machine.Resident, Profile: []int{-9, -12}})
	after = projected(h).Guard
	if after.TierClockSeq != failure.Seq || after.CleanS != 0 || after.TctlMaxC != nil || after.TctlMaxSeq != 0 {
		t.Fatalf("clock failure retained old exposure or peak: %+v", after)
	}
	_, next := h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: 120, TctlMaxC: new(81)})
	after = projected(h).Guard
	if after.CleanS != 120 || after.TctlMaxC == nil || *after.TctlMaxC != 81 || after.TctlMaxSeq != next.Seq {
		t.Fatalf("new counted peak: %+v", after)
	}
}

func TestTemperatureCountsOnlyCleanResidentPasses(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseDone, offset: -10}, coreStart{phase: journal.PhaseDone, offset: -12})
	tr := Trial{Regime: machine.R6, Cores: []int{0, 1}, Condition: machine.Resident, Phase: journal.PhaseGuard, DurationS: 120, Profile: []int{-10, -12}}
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: 120, TctlMaxC: new(99)})
	h.add(&journal.ProfileChange{To: []int{-10, -12}})
	for _, tc := range []struct {
		condition machine.Condition
		outcome   journal.Outcome
	}{{machine.Masked, journal.OutcomePass}, {machine.Isolated, journal.OutcomePass}, {machine.Resident, journal.OutcomeFailure}, {machine.Resident, journal.OutcomeInconclusive}} {
		tr.Condition = tc.condition
		h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: tc.outcome, DurationS: 120, TctlMaxC: new(98)})
	}
	tr.Condition = machine.Resident
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: 120})
	g := projected(h).Guard
	if g.CleanS != 120 || g.TctlMaxC != nil || g.TctlMaxSeq != 0 {
		t.Fatalf("uncounted or absent temperature supplied peak: %+v", g)
	}
	var first int
	for _, r := range []machine.Regime{machine.R1, machine.R2} {
		tr.Regime = r
		_, end := h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: 120, TctlMaxC: new(84)})
		if first == 0 {
			first = end.Seq
		}
	}
	for range 20 {
		h.s.projectionDirty = true
		g = projected(h).Guard
		if g.TctlMaxC == nil || *g.TctlMaxC != 84 || g.TctlMaxSeq != first || g.CleanS != 360 {
			t.Fatalf("equal peak must cite earliest counted pass: %+v", g)
		}
	}
}
