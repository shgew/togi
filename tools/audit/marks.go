package main

import (
	"fmt"
	"slices"

	"github.com/shgew/togi/internal/journal"
)

func (a *auditor) mark(core int, offset *int, seq int) {
	if offset == nil {
		return
	}
	old, ok := a.points[core]
	if !ok || *offset > old.offset {
		a.points[core] = point{*offset, seq}
	}
}
func (a *auditor) resetMarks(core int) {
	delete(a.points, core)
	for id, c := range a.combinations {
		if slices.ContainsFunc(c.members, func(m journal.CombinationMember) bool { return m.Core == core }) {
			delete(a.combinations, id)
		}
	}
	delete(a.reset, core)
}
func (a *auditor) foldMarks(e journal.Event) {
	switch p := e.Data.(type) {
	case *journal.SessionCarried:
		if p.FailurePoints {
			for _, c := range p.Carried {
				a.mark(c.Core, c.FailurePoint, e.Seq)
			}
		}
	case *journal.TrialIntent:
		a.trials[p.Trial] = p
	case *journal.Failure:
		// A carried known failure keeps its source trial ID, which may collide with a local record-only trial.
		if intent := a.trials[p.Trial]; p.KnownFailure != 0 || intent == nil || !intent.RecordOnly {
			if p.Attribution == journal.Attributed && p.Core != nil {
				a.mark(*p.Core, p.Offset, e.Seq)
			}
		}
	case *journal.HuntStart:
		a.hunts[p.Hunt] = slices.Clone(p.Failing)
	case *journal.HuntEnd:
		if p.Result == "culprit" && len(p.Cores) == 1 {
			for i, core := range a.cores {
				if core == p.Cores[0] && i < len(a.hunts[p.Hunt]) {
					a.mark(core, new(a.hunts[p.Hunt][i]), e.Seq)
				}
			}
		}
	case *journal.TunerDecision:
		a.mark(p.Core, p.FailurePoint, e.Seq)
	case *journal.CorePhase:
		// command.reset queues a reset; core.phase restarting search consumes it.
		if a.reset[p.Core] && p.To == journal.PhaseSearch {
			a.resetMarks(p.Core)
		}
		a.mark(p.Core, p.FailurePoint, e.Seq)
	case *journal.CommandReset:
		if p.Core != nil {
			a.reset[*p.Core] = true
		}
	case *journal.Combination:
		a.combinations[p.Combination] = combination{slices.Clone(p.Members), e.Seq}
	}
}

// tuner.md, Failure points and combinations: P[c] <= fail[c]; every P[m] <= C[m].
func (a *auditor) checkProfile(e journal.Event, profile map[int]int, written []int) {
	// An existing applied failure is evidence, not a new application: a partial
	// backoff can only reach a core's failure point by writing that core.
	for _, core := range written {
		p, ok := a.points[core]
		if !ok {
			continue
		}
		offset, known := profile[core]
		if known && offset <= p.offset {
			a.add(e, "avoidance", fmt.Sprintf("core %d offset %d reaches failure point %d recorded at seq %d (tuner.md: P[c] <= fail[c])", core, offset, p.offset, p.seq))
		}
	}
	ids := make([]int, 0, len(a.combinations))
	for id := range a.combinations {
		ids = append(ids, id)
	}
	slices.Sort(ids)
	for _, id := range ids {
		c := a.combinations[id]
		if reachesCombination(profile, written, c.members) {
			a.add(e, "avoidance", fmt.Sprintf("profile reaches combination C%d recorded at seq %d (tuner.md: every P[m] <= C[m])", id, c.seq))
		}
	}
}
func reachesCombination(profile map[int]int, written []int, members []journal.CombinationMember) bool {
	if len(members) == 0 {
		return false
	}
	touched := false
	for _, m := range members {
		offset, known := profile[m.Core]
		if !known || offset > m.Offset {
			return false
		}
		touched = touched || slices.Contains(written, m.Core)
	}
	return touched
}
func (a *auditor) checkOffsets(e journal.Event, offsets []int) {
	for i, o := range offsets {
		if o < -50 || o > 0 {
			a.add(e, "range", fmt.Sprintf("offset[%d]=%d is outside [-50, 0]", i, o))
		}
	}
}

// checkChosenOffsets covers offsets togi chooses. Readbacks and baselines report
// the hardware, which may hold firmware offsets outside togi's range.
func (a *auditor) checkChosenOffsets(e journal.Event) {
	switch p := e.Data.(type) {
	case *journal.SMUIntent:
		a.checkOffsets(e, []int{p.Offset})
	case *journal.TrialIntent:
		a.checkOffsets(e, p.Profile)
		if p.Offset != nil {
			a.checkOffsets(e, []int{*p.Offset})
		}
	case *journal.CorePhase:
		a.checkOffsets(e, []int{p.Offset})
	case *journal.TunerDecision:
		a.checkOffsets(e, []int{p.FromOffset, p.ToOffset})
	case *journal.ProfileChange:
		a.checkOffsets(e, p.To)
	case *journal.DeepeningRound:
		a.checkOffsets(e, p.Base)
		a.checkOffsets(e, p.Target)
		a.checkOffsets(e, p.Profile)
	case *journal.HuntGroup:
		a.checkOffsets(e, p.Profile)
	case *journal.CheckingStep:
		a.checkOffsets(e, p.Profile)
	}
}
func (a *auditor) appliedProfile(e journal.Event, offsets []int) {
	a.checkOffsets(e, offsets)
	profile := make(map[int]int)
	for i, core := range a.cores {
		if i < len(offsets) {
			profile[core] = offsets[i]
		}
	}
	a.checkProfile(e, profile, a.cores)
}
