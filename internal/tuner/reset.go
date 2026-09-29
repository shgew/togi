package tuner

import (
	"fmt"
	"slices"
	"strings"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

const queuedReset = "reset"

func (s *State) queuedReset() (Action, bool) {
	for _, c := range s.cores {
		if c.queued != queuedReset {
			continue
		}
		if s.hunt != nil && s.hunt.end == nil {
			return Action{Kind: Decide, Payload: &journal.HuntEnd{Hunt: s.hunt.start.Hunt, Result: "cancelled", Masks: len(s.hunt.masks), Reason: fmt.Sprintf("core %02d was reset", c.id)}, Cause: []int{c.queueSeq}}, true
		}
		if s.round != nil {
			return Action{Kind: Decide, Payload: &journal.RefineRound{Round: s.round.start.Round, Event: journal.RotationEnd, Reason: fmt.Sprintf("core %02d was reset", c.id)}, Cause: []int{c.queueSeq}}, true
		}
		if s.guard.open {
			return Action{Kind: Decide, Payload: &journal.GuardRotation{Rotation: s.guard.rotation, Event: journal.RotationEnd, Reason: fmt.Sprintf("core %02d was reset", c.id)}, Cause: []int{c.queueSeq}}, true
		}
		var cleared []int
		for _, mark := range s.marks {
			for _, member := range mark.Members {
				if member.Core == c.id {
					cleared = append(cleared, mark.Mark)
					break
				}
			}
		}
		labels := make([]string, len(cleared))
		for i, m := range cleared {
			labels[i] = fmt.Sprintf("J%d", m)
		}
		what := "failed mark"
		if len(labels) > 0 {
			what += " and joint marks " + strings.Join(labels, ", ")
		}
		reason := fmt.Sprintf("reset: %s cleared; search restarts from the baseline %d", what, c.baseline)
		return Action{Kind: Decide, Payload: &journal.CorePhase{Core: c.id, From: c.phase, To: journal.PhaseSearch, Offset: machine.ClampOffset(c.baseline), ClearedJoint: slices.Clone(cleared), Reason: reason}, Cause: []int{c.queueSeq}}, true
	}
	return Action{}, false
}
