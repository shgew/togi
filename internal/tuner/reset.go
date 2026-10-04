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
			return Action{Kind: Decide, Payload: &journal.HuntEnd{Hunt: s.hunt.start.Hunt, Result: "cancelled", Groups: len(s.hunt.groups), Reason: fmt.Sprintf("core %02d was reset", c.id)}, Cause: []int{c.queueSeq}}, true
		}
		if s.round != nil {
			return Action{Kind: Decide, Payload: &journal.DeepeningRound{Round: s.round.start.Round, Event: journal.CycleEnd, Reason: fmt.Sprintf("core %02d was reset", c.id)}, Cause: []int{c.queueSeq}}, true
		}
		if s.checking.open {
			return Action{Kind: Decide, Payload: &journal.CheckingCycle{Cycle: s.checking.cycle, Event: journal.CycleEnd, Reason: fmt.Sprintf("core %02d was reset", c.id)}, Cause: []int{c.queueSeq}}, true
		}
		var cleared []int
		for _, combination := range s.combinations {
			for _, member := range combination.Members {
				if member.Core == c.id {
					cleared = append(cleared, combination.Combination)
					break
				}
			}
		}
		labels := make([]string, len(cleared))
		for i, m := range cleared {
			labels[i] = fmt.Sprintf("C%d", m)
		}
		what := "failure point"
		if len(labels) > 0 {
			what += " and combinations " + strings.Join(labels, ", ")
		}
		reason := fmt.Sprintf("reset: %s cleared; search restarts from the baseline %d", what, c.baseline)
		return Action{Kind: Decide, Payload: &journal.CorePhase{Core: c.id, From: c.phase, To: journal.PhaseSearch, Offset: machine.ClampOffset(c.baseline), ClearedCombination: slices.Clone(cleared), Reason: reason}, Cause: []int{c.queueSeq}}, true
	}
	return Action{}, false
}
