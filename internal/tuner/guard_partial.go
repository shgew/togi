package tuner

import (
	"fmt"
	"slices"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func (s *State) startGuardStep(step int) *journal.GuardStep {
	g := &s.guard
	p := &journal.GuardStep{Rotation: g.rotation, Step: step + 1, Profile: slices.Clone(g.profile)}
	parts := s.parts
	if len(parts) > 1 {
		parts = parts[:len(parts)-1]
	}
	for _, part := range parts {
		shallowest := -50
		for _, id := range part {
			shallowest = max(shallowest, g.profile[s.index(id)])
		}
		partial := journal.GuardPartial{CCD: s.ccd[part[0]]}
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

func (s *State) recordGuardStep(e journal.Event, p *journal.GuardStep) {
	g := &s.guard
	if p.Rotation != g.rotation || !g.open {
		return
	}
	if g.partial == nil {
		g.partial = map[int]*guardStep{}
	}
	g.partial[p.Step] = &guardStep{start: p, seq: e.Seq, completed: map[trialClass]int{}}
	g.lastSeq = e.Seq
	s.projectionDirty = true
}

func (s *State) recordPartialEnd(e journal.Event, p *journal.TrialIntent, end *journal.TrialEnd) {
	g := &s.guard
	step := g.partial[p.Step]
	if p.Rotation != g.rotation || step == nil {
		return
	}
	if end.Outcome == journal.OutcomePass || end.Outcome == journal.OutcomeFailure {
		step.completed[classOf(p)]++
	}
	g.lastSeq = e.Seq
	s.projectionDirty = true
}

func (s *State) partialRequirement(step int, full requirement) (trialClass, []int, bool) {
	started := s.guard.partial[step+1]
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
		short := full.class.withDuration(s.durations.StartS)
		short.cores = coresKey(partial.Cores)
		long := short.withDuration(s.longS(full.cores))
		needed := 3
		if long == short {
			needed++
		}
		if started.completed[short] < needed {
			return short, partial.Cores, true
		}
		if long != short && started.completed[long] < 1 {
			return long, partial.Cores, true
		}
	}
	return trialClass{}, nil, false
}

func (s *State) partialNext(step int, full requirement) (Action, bool) {
	k, cores, pending := s.partialRequirement(step, full)
	if !pending {
		return Action{}, false
	}
	g := &s.guard
	t := Trial{Regime: machine.R7, Workload: k.workload, Cores: slices.Clone(cores), DurationS: k.duration, Phase: journal.PhaseGuard, Condition: machine.Resident, Rotation: g.rotation, RecordOnly: true, Step: step + 1}
	if s.retry != nil && s.retry.RecordOnly && s.retry.Rotation == g.rotation && s.retry.Step == t.Step && s.retry.Workload == k.workload && s.retry.DurationS == k.duration && slices.Equal(s.retry.Cores, cores) {
		t = *s.retry
	}
	return Action{Kind: RunTrial, Trial: t, Cause: []int{g.partial[step+1].seq, g.lastSeq}}, true
}
