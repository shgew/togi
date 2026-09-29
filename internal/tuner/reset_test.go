package tuner

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestResetClearsJointAndReevaluatesOtherCores(t *testing.T) {
	h := newHarness(t,
		coreStart{phase: journal.PhaseDone, offset: -30, fail: new(-31)},
		coreStart{phase: journal.PhaseDone, offset: -30},
	)
	h.add(&journal.SessionBaseline{Offsets: []int{-10, -10}})
	h.add(&journal.MarkJoint{Mark: 1, Hunt: 1, Members: []journal.JointMember{{Core: 0, Offset: -30}, {Core: 1, Offset: -31}}})
	h.add(&journal.CommandReset{Core: new(0)})
	a := h.next()
	reset, ok := a.Payload.(*journal.CorePhase)
	if !ok || reset.To != journal.PhaseSearch || reset.Offset != -10 || cmp.Diff([]int{1}, reset.ClearedJoint) != "" {
		t.Fatalf("reset action %+v", a)
	}
	h.decide(a)
	a = h.next()
	other, ok := a.Payload.(*journal.CorePhase)
	if !ok || other.Core != 1 || other.From != journal.PhaseDone || other.To != journal.PhaseResident {
		t.Fatalf("other core remains incorrectly done after joint reset: %+v", a)
	}
}

func TestResetAfterHuntEndDropsCommitment(t *testing.T) {
	h := residentHarness(t, -29, -30)
	h.add(&journal.HuntStart{Hunt: 1, Failure: 10, Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, DurationS: 120, Failing: []int{-29, -30}, Anchor: []int{0, 0}, Candidates: []int{0, 1}, Starts: 5, StartS: 120})
	h.add(&journal.HuntEnd{Hunt: 1, Result: "culprit", Cores: []int{0}, Masks: 2})
	h.add(&journal.CommandReset{Core: new(0)})
	a := h.next()
	p, ok := a.Payload.(*journal.CorePhase)
	if !ok || p.Core != 0 || p.To != journal.PhaseSearch {
		t.Fatalf("reset action %+v", a)
	}
	h.decide(a)
	if h.s.hunt != nil {
		t.Fatal("ended hunt survived the core reset")
	}
	a = h.next()
	if d, ok := a.Payload.(*journal.TunerDecision); ok && d.Phase == journal.PhaseHunt {
		t.Fatalf("reset core received hunt commitment: %+v", a)
	}
	if _, ok := a.Payload.(*journal.MarkJoint); ok {
		t.Fatalf("reset core received joint mark: %+v", a)
	}
}
