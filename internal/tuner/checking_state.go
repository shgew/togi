package tuner

import (
	"slices"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func (s *State) CheckingCoverage() (bool, []string) {
	if s.checking.open {
		return s.fullCycleCoverage(s.checking.steps)
	}
	return s.fullCycleCoverage(s.steps)
}

func (s *State) projectChecking() *journal.CheckingState {
	g := &s.checking
	if g.profileSeq == 0 {
		return nil
	}
	if !s.projectionDirty && s.projectedChecking != nil {
		return s.projectedChecking
	}
	peak, peakSeq := 0, 0
	for _, entries := range s.ledger {
		for _, e := range entries {
			if e.seq > g.profileSeq && e.pass && e.condition == machine.Together && e.hasTctl &&
				(peakSeq == 0 || e.tctlMax > peak || e.tctlMax == peak && e.seq < peakSeq) {
				peak, peakSeq = e.tctlMax, e.seq
			}
		}
	}
	lastCleanCycle := 0
	for _, q := range s.passedFullCycles {
		if s.eligibleCleanCycle(q) {
			lastCleanCycle = q.cycle
		}
	}
	fullCycleCoverage, missing := s.fullCycleCoverage(s.steps)
	if g.open {
		fullCycleCoverage, missing = s.fullCycleCoverage(g.steps)
	}
	if g.open {
		g.stepsDone = len(g.steps)
		for i := range g.steps {
			unmet := false
			if g.steps[i] == machine.R7 && g.partial[i+1] == nil {
				g.stepsDone = i
				break
			}
			for _, q := range s.requirements(i) {
				if q.class.regime == machine.R7 {
					if _, _, pending := s.partialRequirement(i, q); pending {
						unmet = true
						break
					}
				}
				if q.count > 0 && s.passes(q.class, g.profile, g.startSeq, cycleEvidence) < q.count {
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
	out := &journal.CheckingState{Cycle: g.cycle, CycleOpen: g.open, Steps: slices.Clone(g.steps), StepsDone: g.stepsDone, Profile: slices.Clone(g.profile), ProfileSeq: g.profileSeq, Full: fullCycleCoverage, Missing: missing, CleanCycles: s.CleanCycles(), LastCleanCycle: lastCleanCycle, TctlMaxSeq: peakSeq}
	if peakSeq != 0 {
		out.TctlMaxC = new(peak)
	}
	type exposureKey struct {
		regime   machine.Regime
		workload string
	}
	trials := map[exposureKey]int{}
	for k, entries := range s.ledger {
		valid, checked := 0, false
		for _, e := range entries {
			if !e.pass || e.carried || e.condition == machine.Alone || !atLeastDeep(e.profile, g.profile) {
				continue
			}
			if !checked {
				valid, checked = s.latestFailure(k, g.profile, 0), true
			}
			if e.seq > valid {
				trials[exposureKey{k.regime, k.workload}]++
			}
		}
	}
	for _, r := range machine.Regimes {
		for _, w := range machine.Workloads(r) {
			if n := trials[exposureKey{r, w.ID}]; n > 0 {
				out.Exposure = append(out.Exposure, journal.ExposureRow{Regime: r, Workload: w.ID, Trials: n})
			}
		}
	}
	s.projectedChecking = out
	return out
}
