package tuner

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

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
			h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationEnd, Clean: true, Qualifying: true})
			h.add(&journal.ProfileChange{From: []int{-10, -12}, To: []int{-11, -11}})
			for _, p := range tt.after {
				h.add(p)
			}
			h.add(&journal.ProfileChange{From: []int{-11, -11}, To: tt.final})
			if got := h.s.QualifiedRotations(); got != tt.want {
				t.Fatalf("qualified rotations = %d, want %d", got, tt.want)
			}
		})
	}
}

func TestCreditedRotationReplays(t *testing.T) {
	h := residentHarness(t, -10, -12)
	h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationStart, Steps: []machine.Regime{machine.R1}})
	h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationEnd, Clean: true, Qualifying: true})
	h.add(&journal.ProfileChange{From: []int{-10, -12}, To: []int{-11, -12}})
	h.add(&journal.ProfileChange{From: []int{-11, -12}, To: []int{-10, -12}})
	state := projected(h)
	if state.Guard.CleanRotations != 1 || state.Guard.LastQualifiedRotation != 1 {
		t.Fatalf("lost credited rotation: %+v", state.Guard)
	}
	replayed := New()
	for _, event := range h.events {
		replayed.Fold(event)
	}
	var got journal.State
	replayed.Project(&got)
	if diff := cmp.Diff(state.Guard, got.Guard); diff != "" {
		t.Fatalf("credited rotation after replay (-want +got):\n%s", diff)
	}
}

func TestQualifiedRotationSummaryAfterDeepening(t *testing.T) {
	h := residentHarness(t, -10)
	for _, rotation := range []int{3, 7} {
		h.add(&journal.GuardRotation{Rotation: rotation, Event: journal.RotationStart, Steps: []machine.Regime{machine.R1}})
		h.add(&journal.GuardRotation{Rotation: rotation, Event: journal.RotationEnd, Clean: true, Qualifying: true})
	}
	g := projected(h).Guard
	if g.CleanRotations != 2 || g.LastQualifiedRotation != 7 {
		t.Fatalf("qualified rotation summary: %+v", g)
	}
	h.add(&journal.ProfileChange{From: []int{-10}, To: []int{-11}})
	g = projected(h).Guard
	if g.CleanRotations != 0 || g.LastQualifiedRotation != 0 {
		t.Fatalf("deepening retained shallower rotation credit: %+v", g)
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
}

func TestTemperaturePeakSinceLastProfileChange(t *testing.T) {
	h := residentHarness(t, -10, -12)
	tr := Trial{Regime: machine.R6, Cores: []int{0, 1}, Condition: machine.Resident, Phase: journal.PhaseGuard, DurationS: 120}
	_, peak := h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: 120, TctlMaxC: new(92)})
	before := projected(h).Guard
	if before.TctlMaxC == nil || *before.TctlMaxC != 92 || before.TctlMaxSeq != peak.Seq {
		t.Fatalf("missing resident peak: %+v", before)
	}
	h.add(&journal.ProfileChange{From: []int{-10, -12}, To: []int{-9, -12}})
	after := projected(h).Guard
	if after.TctlMaxC != nil || after.TctlMaxSeq != 0 {
		t.Fatalf("profile change retained old peak: %+v", after)
	}
	_, next := h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: 120, TctlMaxC: new(81)})
	h.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Condition: machine.Resident, Profile: []int{-9, -12}})
	after = projected(h).Guard
	if after.TctlMaxC == nil || *after.TctlMaxC != 81 || after.TctlMaxSeq != next.Seq {
		t.Fatalf("failure without profile change cleared resident peak: %+v", after)
	}
}

func TestTemperatureCountsOnlyResidentPasses(t *testing.T) {
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
	if g.TctlMaxC != nil || g.TctlMaxSeq != 0 {
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
	g = projected(h).Guard
	if g.TctlMaxC == nil || *g.TctlMaxC != 84 || g.TctlMaxSeq != first {
		t.Fatalf("equal peak must cite earliest counted pass: %+v", g)
	}
}
