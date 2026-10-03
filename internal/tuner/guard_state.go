package tuner

import (
	"slices"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func (s *State) projectGuard() *journal.GuardState {
	g := &s.guard
	if g.profileSeq == 0 {
		return nil
	}
	if !s.projectionDirty && s.projectedGuard != nil {
		return s.projectedGuard
	}
	peak, peakSeq := 0, 0
	for _, entries := range s.ledger {
		for _, e := range entries {
			if e.seq > g.profileSeq && e.pass && e.condition == machine.Resident && e.hasTctl &&
				(peakSeq == 0 || e.tctlMax > peak || e.tctlMax == peak && e.seq < peakSeq) {
				peak, peakSeq = e.tctlMax, e.seq
			}
		}
	}
	lastQualified := 0
	for _, q := range s.qualified {
		if q.allDone && (q.seq > s.lastDeepenSeq || s.uncontradicted(q)) {
			lastQualified = q.rotation
		}
	}
	qualifying, missing := s.qualifying(s.steps)
	if g.open {
		qualifying, missing = s.qualifying(g.steps)
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
				if q.count > 0 && s.passes(q.class, g.profile, g.startSeq, rotationEvidence) < q.count {
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
	out := &journal.GuardState{Rotation: g.rotation, RotationOpen: g.open, Steps: slices.Clone(g.steps), StepsDone: g.stepsDone, Profile: slices.Clone(g.profile), ProfileSeq: g.profileSeq, Qualifying: qualifying, Missing: missing, CleanRotations: s.QualifiedRotations(), LastQualifiedRotation: lastQualified, TctlMaxSeq: peakSeq}
	if peakSeq != 0 {
		out.TctlMaxC = new(peak)
	}
	type exposureKey struct {
		regime   machine.Regime
		workload string
	}
	starts := map[exposureKey]int{}
	for k, entries := range s.ledger {
		valid, checked := 0, false
		for _, e := range entries {
			if !e.pass || e.carried || e.condition == machine.Isolated || !atLeastDeep(e.profile, g.profile) {
				continue
			}
			if !checked {
				valid, checked = s.latestFailure(k, g.profile, 0), true
			}
			if e.seq > valid {
				starts[exposureKey{k.regime, k.workload}]++
			}
		}
	}
	for _, r := range machine.Regimes {
		for _, w := range machine.Workloads(r) {
			if n := starts[exposureKey{r, w.ID}]; n > 0 {
				out.Exposure = append(out.Exposure, journal.ExposureRow{Regime: r, Workload: w.ID, Starts: n})
			}
		}
	}
	s.projectedGuard = out
	return out
}
