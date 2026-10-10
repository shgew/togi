package tuner

import (
	"slices"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

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
		for i := len(entries) - 1; i >= 0 && entries[i].seq > g.profileSeq; i-- {
			e := &entries[i]
			if e.pass && e.condition == machine.Together && e.hasTctl &&
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
	stepsDone := g.stepsDone
	if g.open {
		stepsDone = s.checkingStepsDone()
	}
	out := &journal.CheckingState{Cycle: g.cycle, CycleOpen: g.open, Steps: slices.Clone(g.steps), StepsDone: stepsDone, Profile: slices.Clone(g.profile), ProfileSeq: g.profileSeq, Full: fullCycleCoverage, Missing: missing, CleanCycles: s.CleanCycles(), LastCleanCycle: lastCleanCycle, TctlMaxSeq: peakSeq}
	if peakSeq != 0 {
		out.TctlMaxC = new(peak)
	}
	type exposureKey struct {
		regime   machine.Regime
		workload string
	}
	trials := map[exposureKey]int{}
	if s.exposure == nil || s.exposureProfileSeq != g.profileSeq {
		s.exposure, s.exposureProfileSeq = map[trialClass]int{}, g.profileSeq
	}
	for k, entries := range s.ledger {
		n, ok := s.exposure[k]
		if !ok {
			n = s.computeExposure(k, entries)
			s.exposure[k] = n
		}
		if n > 0 {
			trials[exposureKey{k.regime, k.workload}] += n
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

// computeExposure counts class k's current passing trials at least as deep as the checking profile since the
// latest failure that invalidates them; projectChecking memoizes it per class.
func (s *State) computeExposure(k trialClass, entries []entry) int {
	g := &s.checking
	n, valid, checked := 0, 0, false
	for i := range entries {
		e := &entries[i]
		if !e.pass || e.carried || e.condition == machine.Alone || !AtLeastDeep(e.profile, g.profile) || !s.current(e) {
			continue
		}
		if !checked {
			valid, checked = s.latestFailure(k, g.profile, 0), true
		}
		if e.seq > valid {
			n++
		}
	}
	return n
}

func (s *State) checkingStepsDone() int {
	g := &s.checking
	for i := range g.steps {
		if g.steps[i] == machine.R7 && !s.r7ChainsComplete(i) {
			return i
		}
		for _, q := range s.requirements(i) {
			if q.count > 0 && s.cyclePasses(q.class) < q.count {
				return i
			}
		}
	}
	return len(g.steps)
}

// cyclePasses is passes(k, ...) for the open cycle: at the checking profile, since the cycle started, under
// cycleEvidence. It is memoized until the checking cycle or profile, or the evidence, changes.
func (s *State) cyclePasses(k trialClass) int {
	if s.cycleCheckingEpoch != s.checkingEpoch || s.cycleEvidenceEpoch != s.evidenceEpoch || s.cycleMemo == nil {
		if s.cycleMemo == nil {
			s.cycleMemo = map[trialClass]int{}
		}
		clear(s.cycleMemo)
		s.cycleCheckingEpoch, s.cycleEvidenceEpoch = s.checkingEpoch, s.evidenceEpoch
	}
	if n, ok := s.cycleMemo[k]; ok {
		return n
	}
	g := &s.checking
	n := s.passes(k, g.profile, g.startSeq, cycleEvidence)
	s.cycleMemo[k] = n
	return n
}
