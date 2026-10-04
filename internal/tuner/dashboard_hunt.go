package tuner

import (
	"github.com/shgew/togi/internal/journal"
	"slices"
)

func (s *State) groupCarried(h *hunt, g groupRecord) bool {
	_, failure, seqs := s.groupEvidence(h, g, true)
	seqs = append(seqs, failure)
	seqs = append(seqs, g.cause...)
	return slices.ContainsFunc(seqs, func(seq int) bool { _, ok := s.carriedSources[seq]; return ok })
}

// HuntPlan projects the active split and member ladder from the hunt scheduler.
func (s *State) HuntPlan() *HuntPlan {
	h := s.hunt
	if h == nil {
		return nil
	}
	out := &HuntPlan{Number: h.start.Hunt, Regime: h.start.Regime, FailureSeq: h.start.Failure, Trial: h.start.Trial, Candidates: slices.Clone(h.start.Candidates)}
	out.Rerun = s.rerunPlan(h.class)
	running := s.inFlight()
	for _, g := range h.groups {
		m := g.payload
		v := HuntGroup{Number: m.Group, Cores: slices.Clone(m.Cores), Profile: slices.Clone(m.Profile), Stage: m.Stage, Held: slices.Clone(m.Held), Outcome: s.groupOutcome(h, g), Needed: h.start.Trials, Carried: s.groupCarried(h, g), Running: running != nil && running.Hunt == h.start.Hunt && running.Group == m.Group}
		v.Passed = s.passes(h.class.withDuration(m.DurationS), m.Profile, s.inferenceSince(m, g.seq), huntEvidence)
		if m.Probe != nil {
			v.Probe = new(*m.Probe)
		}
		out.Groups = append(out.Groups, v)
	}
	plan, pending := s.nextGroupPlan(h)
	if len(h.groups) > 0 {
		last := h.groups[len(h.groups)-1]
		if s.groupOutcome(h, last) == "running" || last.payload.Probe != nil {
			plan = planOf(last.payload)
		}
	}
	if plan.stage == "part" || plan.stage == "complement" {
		for i := range s.split(h, plan.set, plan.g) {
			cores := s.groupPart(s.split(h, plan.set, plan.g), plan.stage, i, plan.set)
			part := HuntPart{Failing: slices.Clone(cores), Trials: h.start.Trials, DurationS: plan.duration}
			for _, id := range s.ids() {
				if !slices.Contains(cores, id) {
					part.Parked = append(part.Parked, id)
				}
			}
			for j := len(h.groups) - 1; j >= 0; j-- {
				m := h.groups[j].payload
				if m.Stage == plan.stage && m.Granularity == plan.g && m.DurationS == plan.duration && slices.Equal(m.Set, plan.set) && m.Index == i {
					part.Group = m.Group
					part.Outcome = out.Groups[j].Outcome
					part.Running = out.Groups[j].Running
					break
				}
			}
			out.Parts = append(out.Parts, part)
		}
	} else if plan.stage == "full" {
		part := HuntPart{Failing: slices.Clone(plan.set), Trials: h.start.Trials, DurationS: plan.duration}
		for _, id := range s.ids() {
			if !slices.Contains(plan.set, id) {
				part.Parked = append(part.Parked, id)
			}
		}
		if len(h.groups) > 0 {
			m := h.groups[len(h.groups)-1]
			if m.payload.Stage == "full" {
				part.Group = m.payload.Group
				part.Outcome = s.groupOutcome(h, m)
				part.Running = out.Groups[len(out.Groups)-1].Running
			}
		}
		out.Parts = append(out.Parts, part)
	}
	probing := slices.ContainsFunc(h.groups, func(g groupRecord) bool { return g.payload.Probe != nil })
	if probing || (!pending && plan.result && len(plan.set) > 1 && plan.anyFailed) {
		evidence := *h
		if len(h.groups) > 0 && s.groupOutcome(h, h.groups[len(h.groups)-1]) == "running" {
			evidence.groups = h.groups[:len(h.groups)-1]
		}
		next, members, _, has := s.nextMemberProbe(&evidence, plan)
		for _, id := range plan.set {
			v := MemberProbe{Member: id, Offset: h.start.Failing[s.index(id)]}
			v.FailedAt = append(v.FailedAt, v.Offset)
			for j, g := range h.groups {
				m := g.payload
				if m.Probe == nil || m.Probe.Core != id {
					continue
				}
				v.Offset = m.Probe.Offset
				switch out.Groups[j].Outcome {
				case "pass":
					v.PassedAt = append(v.PassedAt, v.Offset)
					if out.Groups[j].Carried {
						v.Carried = append(v.Carried, v.Offset)
					}
				case "failure":
					v.FailedAt = append(v.FailedAt, v.Offset)
				}
				v.Running = out.Groups[j].Running
			}
			if has && next.probe.Core == id && !v.Running {
				v.Offset = next.probe.Offset
			}
			v.Done = slices.ContainsFunc(members, func(m journal.CombinationMember) bool { return m.Core == id }) || (has && slices.ContainsFunc(next.held, func(m journal.CombinationMember) bool { return m.Core == id }) && slices.Index(plan.set, id) < slices.Index(plan.set, next.probe.Core))
			if v.Done {
				for _, m := range members {
					if m.Core == id {
						v.Offset = m.Offset
					}
				}
				for _, m := range next.held {
					if m.Core == id {
						v.Offset = m.Offset
					}
				}
			}
			out.Probes = append(out.Probes, v)
		}
	}
	return out
}
