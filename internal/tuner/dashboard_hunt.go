package tuner

import (
	"github.com/shgew/togi/internal/journal"
	"slices"
)

// groupCarried reports whether the evidence answering this group came from an earlier session. Causes the group
// inherits from earlier groups do not count.
func (s *State) groupCarried(h *hunt, g groupRecord) bool {
	_, failure, seqs := s.groupEvidence(h, g, true)
	if g.payload.Inferred != "" {
		seqs, failure = s.inferredEvidence(h, g)
	}
	seqs = append(seqs, failure)
	return slices.ContainsFunc(seqs, func(seq int) bool { _, ok := s.carriedSources[seq]; return ok })
}

// inferredEvidence finds the trials that answered a group before it was planned, as planGroup found them.
func (s *State) inferredEvidence(h *hunt, g groupRecord) ([]int, int) {
	k := h.class.withDuration(g.payload.DurationS)
	since := s.inferenceSince(g.payload, h.seq)
	if g.payload.Inferred == "pass" {
		seqs := slices.DeleteFunc(s.passSeqs(k, g.payload.Profile, since, huntEvidence), func(seq int) bool { return seq > g.seq })
		return seqs[:min(len(seqs), h.start.Trials)], 0
	}
	return nil, s.failingSeq(k, g.payload.Profile, since)
}

// HuntPlan projects the active split and member ladder from the hunt scheduler.
func (s *State) HuntPlan() *HuntPlan {
	h := s.hunt
	if h == nil {
		return nil
	}
	out := &HuntPlan{Number: h.start.Hunt, Regime: h.start.Regime, FailureSeq: h.start.Failure, Trial: h.start.Trial, Candidates: slices.Clone(h.start.Candidates), Rerun: s.rerunPlan(h.class)}
	out.Groups = s.huntGroups(h)
	plan, pending := s.nextGroupPlan(h)
	if len(h.groups) > 0 {
		last := h.groups[len(h.groups)-1]
		if s.groupOutcome(h, last) == "running" || last.payload.Probe != nil {
			plan = planOf(last.payload)
		}
	}
	out.Parts = s.huntParts(h, plan, out.Groups)
	probing := slices.ContainsFunc(h.groups, func(g groupRecord) bool { return g.payload.Probe != nil })
	if probing || (!pending && plan.result && len(plan.set) > 1 && plan.anyFailed) {
		out.Probes = s.huntProbes(h, plan, out.Groups)
	}
	return out
}

func (s *State) huntGroups(h *hunt) []HuntGroup {
	running := s.inFlight()
	groups := make([]HuntGroup, 0, len(h.groups))
	for _, g := range h.groups {
		m := g.payload
		v := HuntGroup{Number: m.Group, Cores: slices.Clone(m.Cores), Profile: slices.Clone(m.Profile), Stage: m.Stage, Held: slices.Clone(m.Held), Outcome: s.groupOutcome(h, g), Needed: h.start.Trials, Carried: s.groupCarried(h, g), Running: running != nil && running.Hunt == h.start.Hunt && running.Group == m.Group}
		v.Passed = s.passes(h.class.withDuration(m.DurationS), m.Profile, s.inferenceSince(m, g.seq), huntEvidence)
		if m.Probe != nil {
			v.Probe = new(*m.Probe)
		}
		groups = append(groups, v)
	}
	return groups
}

func (s *State) huntPart(h *hunt, plan groupPlan, cores []int) HuntPart {
	part := HuntPart{Failing: slices.Clone(cores), Trials: h.start.Trials, DurationS: plan.duration}
	for _, id := range s.ids() {
		if !slices.Contains(cores, id) {
			part.Parked = append(part.Parked, id)
		}
	}
	return part
}

func (s *State) huntParts(h *hunt, plan groupPlan, groups []HuntGroup) []HuntPart {
	var out []HuntPart
	if plan.singleCorePrior[0] > 0 || len(h.groups) == 1 && plan.stage == "part" && plan.g > 2 && plan.g == len(plan.set) {
		// A corroborated core runs alone before any halves; if it passes, the split starts over from the halves.
		part := s.huntPart(h, plan, plan.cores)
		if len(h.groups) == 1 {
			part.Group, part.Outcome, part.Running = h.groups[0].payload.Group, groups[0].Outcome, groups[0].Running
		}
		return append(out, part)
	}
	switch plan.stage {
	case "part", "complement":
		split := s.split(h, plan.set, plan.g)
		for i := range split {
			part := s.huntPart(h, plan, s.groupPart(split, plan.stage, i, plan.set))
			for j, g := range slices.Backward(h.groups) {
				m := g.payload
				if m.Stage == plan.stage && m.Granularity == plan.g && m.DurationS == plan.duration && slices.Equal(m.Set, plan.set) && m.Index == i {
					part.Group = m.Group
					part.Outcome = groups[j].Outcome
					part.Running = groups[j].Running
					break
				}
			}
			out = append(out, part)
		}
	case "full":
		part := s.huntPart(h, plan, plan.set)
		if len(h.groups) > 0 {
			m := h.groups[len(h.groups)-1]
			if m.payload.Stage == "full" {
				part.Group = m.payload.Group
				part.Outcome = s.groupOutcome(h, m)
				part.Running = groups[len(groups)-1].Running
			}
		}
		out = append(out, part)
	}
	return out
}

func (s *State) huntProbes(h *hunt, plan groupPlan, groups []HuntGroup) []MemberProbe {
	evidence := *h
	if len(h.groups) > 0 && s.groupOutcome(h, h.groups[len(h.groups)-1]) == "running" {
		evidence.groups = h.groups[:len(h.groups)-1]
	}
	var next groupPlan
	var members []journal.CombinationMember
	has := false
	if len(s.checking.profile) == len(s.cores) {
		next, members, _, has = s.nextMemberProbe(&evidence, plan)
	}
	var out []MemberProbe
	for _, id := range plan.set {
		v := s.memberProbe(h, id, groups)
		if has && next.probe.Core == id && !v.Running {
			v.Offset = next.probe.Offset
		}
		v.Done = slices.ContainsFunc(members, func(m journal.CombinationMember) bool { return m.Core == id }) || (has && slices.Index(plan.set, id) < slices.Index(plan.set, next.probe.Core))
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
		out = append(out, v)
	}
	return out
}

func (s *State) memberProbe(h *hunt, id int, groups []HuntGroup) MemberProbe {
	v := MemberProbe{Member: id, Offset: h.start.Failing[s.index(id)]}
	v.FailedAt = append(v.FailedAt, v.Offset)
	for j, g := range h.groups {
		m := g.payload
		if m.Probe == nil || m.Probe.Core != id {
			continue
		}
		v.Offset = m.Probe.Offset
		switch groups[j].Outcome {
		case "pass":
			v.PassedAt = append(v.PassedAt, v.Offset)
			if groups[j].Carried {
				v.Carried = append(v.Carried, v.Offset)
			}
		case "failure":
			v.FailedAt = append(v.FailedAt, v.Offset)
		}
		v.Running = groups[j].Running
	}
	return v
}
