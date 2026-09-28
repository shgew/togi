package tuner

import (
	"fmt"
	"slices"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

const queuedReset = "reset"

// queuedReset starts the reset queued first in scheduling order.
func (s *State) queuedReset() (Action, bool) {
	for _, c := range s.cores {
		if c.queued == queuedReset {
			return Action{Kind: Decide, Payload: reset(c.snapshot(), c.baseline), Cause: []int{c.queueSeq}}, true
		}
	}
	return Action{}, false
}

func reset(c coreState, baseline int) *journal.CorePhase {
	return &journal.CorePhase{
		Core: c.core, From: c.phase, To: journal.PhaseSearch, Offset: machine.ClampOffset(baseline),
		Reason: fmt.Sprintf("reset: failed mark, unproven depth and confirmation cleared; search restarts from the baseline %d", baseline),
	}
}

// regainable is the unproven depth a confirmed core may still regain: never to its settled mark or deeper. Since
// offset - unproven is the confirmed edge or one shallower than the failed mark, regain stays within both.
func (c *core) regainable() int {
	if c.phase != journal.PhaseConfirmed {
		return 0
	}
	n := c.unproven
	if c.settled != nil {
		n = min(n, c.offset-1-*c.settled)
	}
	return max(n, 0)
}

// regainNext regains one count on the first core in scheduling order that has regainable depth and has not regained
// since the clean rotation end that made regains due.
func (s *State) regainNext() (Action, bool) {
	g := &s.guard
	if g.open || g.regainFrom == 0 {
		return Action{}, false
	}
	for _, c := range s.cores {
		if c.regainable() > 0 && !slices.Contains(g.regained, c.id) {
			return Action{Kind: Decide, Payload: regain(c.snapshot(), g.rotation), Cause: []int{g.regainFrom}}, true
		}
	}
	return Action{}, false
}

func regain(c coreState, rotation int) *journal.TunerDecision {
	to := c.offset - 1
	spent := append(slices.Clone(c.spent), to)
	slices.Sort(spent)
	return &journal.TunerDecision{
		Core: c.core, Phase: journal.PhaseGuard, Decision: journal.Regain, FromOffset: c.offset, ToOffset: to,
		Pass: c.pass, FailedMark: c.fail, UnprovenDepth: c.unproven - 1, SettledMark: c.settled, SpentSteps: spent,
		Reason: fmt.Sprintf("rotation %d completed clean: one of %d suspect counts regained, its retry at %d spent; unproven depth %d", rotation, c.unproven, to, c.unproven-1),
	}
}
