package tuner

import (
	"fmt"
	"slices"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type coreState struct {
	core       int
	phase      journal.Phase
	offset     int
	pass, fail *int
	checks     int
}

func (c *core) snapshot() coreState {
	return coreState{c.id, c.phase, c.offset, c.pass, c.fail, c.checks}
}

func (s *State) checkEdge(c coreState, offset int, pass, fail *int, label string) *journal.TunerDecision {
	k := c.checks
	w1 := machine.Workloads(machine.R1)[k%len(machine.Workloads(machine.R1))].ID
	w2 := machine.Workloads(machine.R2)[k%len(machine.Workloads(machine.R2))].ID
	return &journal.TunerDecision{Core: c.core, Phase: journal.PhaseSearch, Decision: journal.CheckEdge, FromOffset: c.offset, ToOffset: offset, Pass: pass, FailedMark: fail, Workloads: []string{w1, w2}, Reason: fmt.Sprintf("candidate edge %d: %s; the full pass rule needs %d starts each of R1 %s and R2 %s", offset, label, s.n, w1, w2)}
}

func (s *State) searchPass(c coreState) journal.Payload {
	o := c.offset
	pass := new(o)
	switch {
	case o == machine.MinOffset:
		return s.checkEdge(c, o, pass, c.fail, "floor")
	case c.fail == nil:
		return &journal.TunerDecision{Core: c.core, Phase: c.phase, Decision: journal.StepDeeper, FromOffset: o, ToOffset: max(o-5, machine.MinOffset), Pass: pass, Reason: "coarse, no failed mark yet"}
	case o-1 == *c.fail:
		return s.checkEdge(c, o, pass, c.fail, fmt.Sprintf("failed mark %d", *c.fail))
	}
	return &journal.TunerDecision{Core: c.core, Phase: c.phase, Decision: journal.StepDeeper, FromOffset: o, ToOffset: o - 1, Pass: pass, FailedMark: c.fail, Reason: fmt.Sprintf("fine, failed mark %d", *c.fail)}
}

func (s *State) searchFailure(c coreState) journal.Payload {
	o := c.offset
	if o == machine.MaxOffset {
		return failedAtZero(c.core)
	}
	fail := o
	if c.fail != nil {
		fail = max(*c.fail, o)
	}
	pass, discarded := keepPass(c.pass, fail)
	if pass == nil {
		return &journal.TunerDecision{Core: c.core, Phase: c.phase, Decision: journal.Backoff, FromOffset: o, ToOffset: min(o+5, machine.MaxOffset), FailedMark: new(fail), Reason: "coarse, no passed step" + discarded}
	}
	if *pass-1 == fail {
		return s.checkEdge(c, *pass, pass, new(fail), fmt.Sprintf("failed mark %d%s", fail, discarded))
	}
	return &journal.TunerDecision{Core: c.core, Phase: c.phase, Decision: journal.Backoff, FromOffset: o, ToOffset: *pass - 1, Pass: pass, FailedMark: new(fail), Reason: fmt.Sprintf("fine, deepest pass %d%s", *pass, discarded)}
}

func keepPass(pass *int, fail int) (*int, string) {
	if pass != nil && *pass <= fail {
		return nil, fmt.Sprintf("; passed step at %d discarded, the failure contradicts it", *pass)
	}
	return pass, ""
}

func failedAtZero(core int) *journal.DeadEnd {
	return &journal.DeadEnd{Condition: journal.DeadEndFailureAtZero, Core: new(core), Detail: fmt.Sprintf("core %02d failed at CO 0; the instability is not caused by Curve Optimizer", core)}
}

func attributeIsolated(intent *journal.TrialIntent, signal machine.Signal) *journal.Failure {
	return &journal.Failure{Signal: signal, Attribution: journal.Attributed, Core: intent.Core, Offset: intent.Offset, Trial: intent.Trial, Profile: slices.Clone(intent.Profile)}
}

func (s *State) perCore() (Action, bool) {
	if s.cursor >= 0 {
		c := s.cores[s.cursor]
		if c.phase == journal.PhaseSearch && !c.check && len(c.stepSeqs) >= 2 {
			return Action{Kind: Decide, Payload: s.searchPass(c.snapshot()), Cause: slices.Clone(c.stepSeqs)}, true
		}
	}
	if s.retry != nil && s.retry.Condition == machine.Isolated {
		return Action{Kind: RunTrial, Trial: *s.retry, Cause: []int{s.core(s.retry.Core).lastSeq}}, true
	}
	if s.cursor >= 0 {
		c := s.cores[s.cursor]
		if c.phase == journal.PhaseSearch && !c.check && c.stepR1 && len(c.stepSeqs) == 1 {
			return s.searchTrial(c, machine.R2, ""), true
		}
	}
	for i := range s.cores {
		c := s.cores[(s.cursor+1+i)%len(s.cores)]
		if c.phase != journal.PhaseSearch {
			continue
		}
		if !c.check {
			return s.searchTrial(c, machine.R1, ""), true
		}
		p := make([]int, len(s.cores))
		p[s.index(c.id)] = c.offset
		cause := []int{c.phaseSeq}
		for j, r := range []machine.Regime{machine.R1, machine.R2} {
			k := trialClass{regime: r, workload: c.checkWorkloads[j], cores: coresKey([]int{c.id}), duration: s.durations.SearchTrialS}
			seqs := s.passSeqs(k, p, c.phaseSeq, edgeEvidence)
			if len(seqs) < s.n {
				return s.searchTrial(c, r, c.checkWorkloads[j]), true
			}
			cause = s.citeCarried(cause, seqs[:s.n]...)
		}
		reason, done := s.done(c, s.offsets())
		phase := journal.PhaseResident
		if done {
			phase = journal.PhaseDone
		} else {
			reason = "one count deeper reaches no mark"
		}
		return Action{Kind: Decide, Payload: &journal.CorePhase{Core: c.id, From: journal.PhaseSearch, To: phase, Offset: c.offset, Pass: new(c.offset), FailedMark: c.fail, Reason: fmt.Sprintf("edge %d passed %d starts of R1 %s and R2 %s%s; %s", c.offset, s.n, c.checkWorkloads[0], c.checkWorkloads[1], s.carriedReason(cause), reason)}, Cause: cause}, true
	}
	return Action{}, false
}

func (s *State) searchTrial(c *core, r machine.Regime, w string) Action {
	return Action{Kind: RunTrial, Trial: Trial{Core: c.id, Offset: c.offset, Regime: r, Phase: journal.PhaseSearch, Condition: machine.Isolated, DurationS: s.durations.SearchTrialS, Workload: w}, Cause: []int{c.lastSeq}}
}
