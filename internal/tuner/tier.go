package tuner

import (
	"fmt"
	"slices"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

const (
	silverCleanS = 24 * 3600
	goldCleanS   = 100 * 3600
)

func (s *State) recomputeTierClock() { s.tierClockDirty = true; s.projectionDirty = true }
func (s *State) ensureTierClock() {
	if !s.tierClockDirty {
		return
	}
	s.tierClockSeq = s.lastDeepenSeq
	for _, f := range s.pendingFailures {
		if atLeastDeep(f.profile, s.guard.profile) {
			s.tierClockSeq = max(s.tierClockSeq, f.seq)
		}
	}
	s.tierClockDirty = false
}

func (s *State) clean() (int, map[machine.Regime]int) {
	s.ensureTierClock()
	total := 0
	regimes := map[machine.Regime]int{}
	for _, entries := range s.ledger {
		for _, e := range entries {
			if e.seq > s.tierClockSeq && e.pass && e.condition == machine.Resident {
				total += e.duration
				regimes[e.class.regime] += e.duration
			}
		}
	}
	return total, regimes
}

func (s *State) tierNext() (Action, bool) {
	target := journal.TierNone
	reason := ""
	for _, c := range s.cores {
		if c.phase == journal.PhaseSearch {
			reason = fmt.Sprintf("core %02d is in search", c.id)
			break
		}
		if c.phase != journal.PhaseDone {
			reason = fmt.Sprintf("core %02d is not done: one count deeper reaches no mark", c.id)
			break
		}
	}
	if reason == "" {
		p := s.guard.profile
		opt := s.best()
		if opt != nil && totalDepth(opt) < totalDepth(p) {
			reason = fmt.Sprintf("refinement can reach %d more counts", totalDepth(p)-totalDepth(opt))
		}
	}
	if reason == "" && s.QualifiedRotations() == 0 {
		if s.lastDeepenSeq > 0 {
			reason = "the profile went deeper"
		} else {
			reason = "no clean qualifying rotation since the profile last went deeper"
		}
	}
	if reason == "" {
		clean, _ := s.clean()
		target = journal.TierBronze
		reason = "every core is done and the profile passed a clean qualifying rotation"
		if clean >= silverCleanS {
			target = journal.TierSilver
			reason = fmt.Sprintf("24 clean hours since the tier clock started at #%d", s.tierClockSeq)
		}
		if clean >= goldCleanS {
			target = journal.TierGold
			reason = fmt.Sprintf("100 clean hours since the tier clock started at #%d", s.tierClockSeq)
		}
	}
	if target == s.tier {
		return Action{}, false
	}
	return Action{Kind: Decide, Payload: &journal.TierChange{From: s.tier, To: target, Reason: reason}, Cause: []int{s.tierCause}}, true
}

func rateBound(cleanS int) *float64 {
	if cleanS <= 0 {
		return nil
	}
	return new(float64((3*3600*10000+cleanS-1)/cleanS) / 10000)
}

func (s *State) projectGuard() *journal.GuardState {
	g := &s.guard
	if g.profileSeq == 0 {
		return nil
	}
	if !s.projectionDirty && s.projectedGuard != nil {
		return s.projectedGuard
	}
	clean, byRegime := s.clean()
	regimes := make([]journal.RegimeClean, len(machine.Regimes))
	for i, r := range machine.Regimes {
		regimes[i] = journal.RegimeClean{Regime: r, CleanS: byRegime[r], RateBoundPerH: rateBound(byRegime[r])}
	}
	qualifying, missing := s.qualifying(s.steps)
	if g.open {
		qualifying, missing = s.qualifying(g.steps)
	}
	if g.open {
		g.stepsDone = len(g.steps)
		for i := range g.steps {
			unmet := false
			for _, q := range s.requirements(i) {
				if q.count > 0 && s.passes(q.class, g.profile, g.startSeq) < q.count {
					unmet = true
					break
				}
			}
			if unmet {
				g.stepsDone = i
				break
			}
		}
	}
	out := &journal.GuardState{Rotation: g.rotation, RotationOpen: g.open, Steps: slices.Clone(g.steps), StepsDone: g.stepsDone, Profile: slices.Clone(g.profile), ProfileSeq: g.profileSeq, Qualifying: qualifying, Missing: missing, TierClockSeq: s.tierClockSeq, CleanRotations: s.QualifiedRotations(), CleanS: clean, Regimes: regimes, RateBoundPerH: rateBound(clean), TctlMaxC: g.tctlMax, TctlMaxSeq: g.tctlSeq}
	valid := map[trialClass]int{}
	for k := range s.ledger {
		valid[k] = s.latestFailure(k, g.profile, 0)
	}
	for _, r := range machine.Regimes {
		for _, w := range machine.Workloads(r) {
			row := journal.ExposureRow{Regime: r, Workload: w.ID}
			for k, entries := range s.ledger {
				if k.regime != r || k.workload != w.ID {
					continue
				}
				for _, e := range entries {
					if !e.pass {
						continue
					}
					if e.condition != machine.Isolated && e.seq > valid[k] && atLeastDeep(e.profile, g.profile) {
						row.Starts++
					}
					if e.condition == machine.Resident && e.seq > s.tierClockSeq {
						row.CleanS += e.duration
					}
				}
			}
			if row.Starts > 0 || row.CleanS > 0 {
				row.RateBoundPerH = rateBound(row.CleanS)
				out.Exposure = append(out.Exposure, row)
			}
		}
	}
	s.projectedGuard = out
	s.projectionDirty = false
	return out
}
