package tuner

import (
	"fmt"
	"slices"

	"code.marleb.org/shgew/shycler/internal/journal"
)

const (
	silverCleanS = 24 * 3600
	goldCleanS   = 100 * 3600
)

var tierReasons = map[journal.Tier]string{
	journal.TierNone:   "the profile changed",
	journal.TierBronze: "every core is confirmed and the profile survived a clean rotation",
	journal.TierSilver: "24 clean hours since the profile change",
	journal.TierGold:   "100 clean hours since the profile change",
}

// tierFor never returns Platinum: it needs field hours from the observe service.
func tierFor(allConfirmed, dirty bool, cleanRotations, cleanS int) journal.Tier {
	switch {
	case !allConfirmed || dirty || cleanRotations == 0:
		return journal.TierNone
	case cleanS >= goldCleanS:
		return journal.TierGold
	case cleanS >= silverCleanS:
		return journal.TierSilver
	}
	return journal.TierBronze
}

func (s *State) tierNext() (Action, bool) {
	g := &s.guard
	unconfirmed := slices.IndexFunc(s.cores, func(c *core) bool { return c.phase != journal.PhaseConfirmed })
	to := tierFor(unconfirmed < 0, g.dirty, g.cleanRotations, g.cleanS)
	if to == s.tier {
		return Action{}, false
	}
	reason := tierReasons[to]
	if unconfirmed >= 0 {
		c := s.cores[unconfirmed]
		reason = fmt.Sprintf("core %02d is in %s", c.id, c.phase)
	}
	return Action{Kind: Decide, Payload: &journal.TierChange{From: s.tier, To: to, Reason: reason}, Cause: []int{s.tierCause}}, true
}

func rateBound(cleanS int) *float64 {
	if cleanS <= 0 {
		return nil
	}
	return new(float64((3*3600*10000+cleanS-1)/cleanS) / 10000)
}
