package tuner

import (
	"fmt"
	"slices"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func (s *State) startCheckingStep(step int) *journal.CheckingStep {
	g := &s.checking
	p := &journal.CheckingStep{Cycle: g.cycle, Step: step + 1, Profile: slices.Clone(g.profile)}
	parts := s.parts
	if len(parts) > 1 {
		parts = parts[:len(parts)-1]
	}
	for _, part := range parts {
		shallowest := -50
		for _, id := range part {
			shallowest = max(shallowest, g.profile[s.index(id)])
		}
		partial := journal.CheckingPartial{CCD: s.ccd[part[0]]}
		for _, id := range part {
			if g.profile[s.index(id)] < shallowest {
				partial.Cores = append(partial.Cores, id)
			}
		}
		if len(partial.Cores) == 0 {
			partial.Reason = fmt.Sprintf("every core is tied at the CCD's shallowest offset %d; no cores to load", shallowest)
		}
		p.Partials = append(p.Partials, partial)
	}
	return p
}

func (s *State) recordCheckingStep(e journal.Event, p *journal.CheckingStep) {
	g := &s.checking
	if p.Cycle != g.cycle || !g.open {
		return
	}
	if g.partial == nil {
		g.partial = map[int]*checkingStep{}
	}
	g.partial[p.Step] = &checkingStep{start: p, seq: e.Seq, completed: map[trialClass]int{}, passed: map[trialClass]int{}, failed: map[trialClass]int{}}
	g.lastSeq = e.Seq
	s.projectionDirty = true
}

func (s *State) recordPartialEnd(e journal.Event, p *journal.TrialIntent, end *journal.TrialEnd) {
	g := &s.checking
	step := g.partial[p.Step]
	if p.Cycle != g.cycle || step == nil {
		return
	}
	if end.Outcome == journal.OutcomePass || end.Outcome == journal.OutcomeFailure {
		step.completed[classOf(p)]++
		if end.Outcome == journal.OutcomePass {
			step.passed[classOf(p)]++
		} else {
			step.failed[classOf(p)]++
		}
	}
	g.lastSeq = e.Seq
	s.projectionDirty = true
}

func (s *State) partialRequirement(step int, full requirement) (trialClass, []int, bool) {
	started := s.checking.partial[step+1]
	if started == nil || len(full.cores) == 0 {
		return trialClass{}, nil, false
	}
	for _, partial := range started.start.Partials {
		if len(partial.Cores) == 0 || s.ccd[full.cores[0]] != partial.CCD {
			continue
		}
		if slices.ContainsFunc(full.cores, func(id int) bool { return s.ccd[id] != partial.CCD }) {
			continue
		}
		for _, q := range s.partialRequirements(full, partial.Cores) {
			if started.completed[q.class] < q.count {
				return q.class, partial.Cores, true
			}
		}
	}
	return trialClass{}, nil, false
}

func (s *State) partialRequirements(full requirement, cores []int) []requirement {
	short := full.class.withDuration(s.durations.ShortTrialS)
	short.cores = coresKey(cores)
	long := short.withDuration(s.longS(full.cores))
	if short == long {
		return []requirement{{class: short, cores: cores, count: 4}}
	}
	return []requirement{{class: short, cores: cores, count: 3}, {class: long, cores: cores, count: 1}}
}

func (s *State) partialNext(step int, full requirement) (Action, bool) {
	k, cores, pending := s.partialRequirement(step, full)
	if !pending {
		return Action{}, false
	}
	g := &s.checking
	t := Trial{Regime: machine.R7, Workload: k.workload, Cores: slices.Clone(cores), DurationS: k.duration, Phase: journal.PhaseChecking, Condition: machine.Together, Cycle: g.cycle, RecordOnly: true, Step: step + 1}
	if s.retry != nil && s.retry.RecordOnly && s.retry.Cycle == g.cycle && s.retry.Step == t.Step && s.retry.Workload == k.workload && s.retry.DurationS == k.duration && slices.Equal(s.retry.Cores, cores) {
		t = *s.retry
	}
	return Action{Kind: RunTrial, Trial: t, Cause: []int{g.partial[step+1].seq, g.lastSeq}}, true
}
