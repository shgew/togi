package tuner

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
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
