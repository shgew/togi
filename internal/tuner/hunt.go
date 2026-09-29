package tuner

import (
	"fmt"
	"slices"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type maskRecord struct {
	payload *journal.HuntMask
	seq     int
}

type hunt struct {
	start                          *journal.HuntStart
	seq                            int
	class                          trialClass
	masks                          []maskRecord
	end                            *journal.HuntEnd
	endSeq, markSeq, directFailure int
}

func (s *State) openHunt(e journal.Event, p *journal.HuntStart) {
	s.hunt = &hunt{start: p, seq: e.Seq, class: trialClass{p.Regime, p.Workload, fmt.Sprint(p.Cores), p.DurationS}}
	s.nextHunt = max(s.nextHunt, p.Hunt)
	s.lastPlanSeq = e.Seq
	if len(s.queue) > 0 && s.queue[0].seq == p.Failure {
		s.queue = s.queue[1:]
	}
}

func (s *State) recordMask(e journal.Event, p *journal.HuntMask) {
	if h := s.hunt; h != nil && p.Hunt == h.start.Hunt {
		h.masks = append(h.masks, maskRecord{p, e.Seq})
	}
}

func (s *State) endHunt(e journal.Event, p *journal.HuntEnd) {
	h := s.hunt
	if h == nil || h.start.Hunt != p.Hunt {
		return
	}
	h.end, h.endSeq = p, e.Seq
	if p.Result == "direct" && len(e.Cause) > 0 {
		h.directFailure = e.Cause[0]
	}
	if p.Result == "cancelled" {
		f := s.failureBySeq(h.start.Failure)
		if f != nil {
			s.queue = append([]pendingFailure{*f}, s.queue...)
		}
		s.hunt = nil
	}
}

func (s *State) failureBySeq(seq int) *pendingFailure {
	if i, ok := s.failureIndex[seq]; ok {
		return &s.pendingFailures[i]
	}
	return nil
}

func (s *State) huntStartNext() Action {
	f := s.queue[0]
	if mark, ok := s.reaches(f.profile); ok {
		return Action{Kind: Decide, Payload: &journal.HuntSkipped{Failure: f.seq, Reason: "its profile reaches " + mark}, Cause: []int{f.seq}}
	}
	anchor := make([]int, len(f.profile))
	anchorSeq := 0
	for i := len(s.qualified) - 1; i >= 0; i-- {
		q := s.qualified[i]
		if len(q.profile) != len(f.profile) || slices.Equal(q.profile, f.profile) || !atLeastShallow(q.profile, f.profile) {
			continue
		}
		if _, marked := s.reaches(q.profile); marked {
			continue
		}
		anchor = slices.Clone(q.profile)
		anchorSeq = q.seq
		break
	}
	var candidates []int
	for i, c := range s.byID() {
		if f.profile[i] < anchor[i] {
			candidates = append(candidates, c.id)
		}
	}
	var cores []int
	for _, id := range s.ids() {
		if f.class.cores == fmt.Sprint([]int{id}) {
			cores = []int{id}
			break
		}
	}
	if cores == nil {
		for _, part := range s.parts {
			if fmt.Sprint(part) == f.class.cores {
				cores = slices.Clone(part)
				break
			}
		}
	}
	if cores == nil {
		cores = s.ids()
	}
	p := &journal.HuntStart{Hunt: s.nextHunt + 1, Failure: f.seq, Trial: f.failure.Trial, Regime: f.class.regime, Workload: f.class.workload, Cores: cores, DurationS: f.class.duration, Failing: slices.Clone(f.profile), Anchor: anchor, AnchorSeq: anchorSeq, Candidates: candidates, Starts: s.n, StartS: s.durations.StartS, Miss: s.evidence.Miss, Rate: s.evidence.Rate, Ranking: slices.Clone(s.ranking)}
	return Action{Kind: Decide, Payload: p, Cause: []int{f.seq, anchorSeq}}
}

func (s *State) split(h *hunt, set []int, g int) [][]int {
	if len(set) == 0 {
		return nil
	}
	g = min(g, len(set))
	if g < 1 {
		g = 1
	}
	parts := make([][]int, 0, g)
	if g == 2 && slices.Equal(set, h.start.Candidates) {
		var loaded, idle []int
		for _, id := range set {
			if slices.Contains(h.start.Cores, id) {
				loaded = append(loaded, id)
			} else {
				idle = append(idle, id)
			}
		}
		if len(loaded) > 0 && len(idle) > 0 {
			parts = append(parts, loaded, idle)
		}
	}
	if len(parts) == 0 {
		for i, at := 0, 0; i < g; i++ {
			size := len(set) / g
			if i < len(set)%g {
				size++
			}
			parts = append(parts, slices.Clone(set[at:at+size]))
			at += size
		}
	}
	if len(s.recent) > 0 {
		slices.SortStableFunc(parts, func(a, b []int) int {
			ar, br := false, false
			for _, id := range a {
				ar = ar || slices.Contains(s.recent, id)
			}
			for _, id := range b {
				br = br || slices.Contains(s.recent, id)
			}
			if ar == br {
				return 0
			}
			if ar {
				return -1
			}
			return 1
		})
	}
	return parts
}

type maskPlan struct {
	set, cores                        []int
	g                                 int
	stage                             string
	index, duration                   int
	escalated, fullChecked, anyFailed bool
	result                            bool
}

func (s *State) nextMaskPlan(h *hunt) (maskPlan, bool) {
	p := maskPlan{set: slices.Clone(h.start.Candidates), g: 2, stage: "part", duration: h.start.StartS}
	if len(p.set) == 0 {
		return p, false
	}
	if len(h.masks) == 0 {
		if len(p.set) == 1 {
			p.result = true
			return p, false
		}
		p.cores = s.split(h, p.set, p.g)[0]
		return p, true
	}
	last := h.masks[len(h.masks)-1]
	m := last.payload
	p = maskPlan{set: slices.Clone(m.Set), g: m.Granularity, stage: m.Stage, index: m.Index, duration: m.DurationS, escalated: m.Escalated, fullChecked: m.FullChecked, anyFailed: m.AnyFailed}
	outcome := s.maskOutcome(h, last)
	if outcome == "running" {
		return p, false
	}
	if outcome == "failure" {
		p.anyFailed = true
		if p.stage == "part" || p.stage == "complement" {
			p.set = slices.Clone(m.Cores)
			if p.stage == "complement" {
				p.g = max(p.g-1, 2)
			} else {
				p.g = 2
			}
			p.stage = "part"
			p.index = 0
			if len(p.set) == 1 {
				p.result = true
				return p, false
			}
			p.cores = s.split(h, p.set, p.g)[0]
			return p, true
		}
		p.fullChecked = true
	}
	if p.stage == "full" {
		p.fullChecked = true
		if outcome == "pass" {
			p.escalated = true
			p.duration = h.start.DurationS
			p.set = slices.Clone(h.start.Candidates)
			p.g = 2
			p.stage = "part"
			p.index = 0
			p.fullChecked = false
			p.anyFailed = false
			if len(p.set) == 1 {
				p.result = true
				return p, false
			}
			p.cores = s.split(h, p.set, p.g)[0]
			return p, true
		}
	}
	if p.stage == "part" || p.stage == "complement" {
		parts := s.split(h, p.set, p.g)
		if p.index+1 < len(parts) {
			p.index++
			p.cores = s.maskPart(parts, p.stage, p.index, p.set)
			return p, true
		}
		if p.stage == "part" && p.g > 2 {
			p.stage = "complement"
			p.index = 0
			p.cores = s.maskPart(parts, p.stage, p.index, p.set)
			return p, true
		}
	}
	if !p.escalated && !p.fullChecked && p.g == 2 && slices.Equal(p.set, h.start.Candidates) && h.start.DurationS != h.start.StartS {
		p.stage = "full"
		p.index = 0
		p.cores = slices.Clone(p.set)
		return p, true
	}
	if p.g < len(p.set) {
		p.g = min(2*p.g, len(p.set))
		p.stage = "part"
		p.index = 0
		p.cores = s.split(h, p.set, p.g)[0]
		return p, true
	}
	p.result = true
	return p, false
}

func (s *State) maskPart(parts [][]int, stage string, index int, set []int) []int {
	if stage == "part" {
		return slices.Clone(parts[index])
	}
	var out []int
	for _, id := range set {
		if !slices.Contains(parts[index], id) {
			out = append(out, id)
		}
	}
	return out
}

func (s *State) maskOutcome(h *hunt, m maskRecord) string {
	if m.payload.Skipped {
		return "skipped"
	}
	if m.payload.Inferred != "" {
		return m.payload.Inferred
	}
	k := h.class.withDuration(m.payload.DurationS)
	if s.fails(k, m.payload.Profile, m.seq) {
		return "failure"
	}
	if s.passes(k, m.payload.Profile, m.seq) >= h.start.Starts {
		return "pass"
	}
	return "running"
}

func (s *State) huntNext() (Action, bool) {
	h := s.hunt
	if h.end != nil {
		return s.huntCommitment(h)
	}
	if len(h.masks) > 0 {
		m := h.masks[len(h.masks)-1]
		if s.maskOutcome(h, m) == "running" {
			if mark, ok := s.reaches(m.payload.Profile); ok {
				return Action{Kind: Decide, Payload: s.makeMask(h, maskPlan{set: m.payload.Set, cores: m.payload.Cores, g: m.payload.Granularity, stage: m.payload.Stage, index: m.payload.Index, duration: m.payload.DurationS, escalated: m.payload.Escalated, fullChecked: m.payload.FullChecked, anyFailed: m.payload.AnyFailed}, m.payload.Mask, "", true, "its profile reaches "+mark), Cause: []int{m.seq}}, true
			}
			if s.retry != nil && s.retry.Hunt == h.start.Hunt && s.retry.Mask == m.payload.Mask {
				return Action{Kind: RunTrial, Trial: *s.retry, Cause: []int{m.seq}}, true
			}
			t := Trial{Regime: h.start.Regime, Workload: h.start.Workload, Condition: machine.Masked, Phase: journal.PhaseHunt, DurationS: m.payload.DurationS, Cores: slices.Clone(h.start.Cores), Profile: slices.Clone(m.payload.Profile), Hunt: h.start.Hunt, Mask: m.payload.Mask}
			return Action{Kind: RunTrial, Trial: t, Cause: []int{m.seq}}, true
		}
	}
	next, has := s.nextMaskPlan(h)
	if !has {
		result := "joint"
		if len(next.set) == 1 {
			result = "culprit"
		} else if !next.anyFailed {
			result = "fallback"
		}
		return Action{Kind: Decide, Payload: &journal.HuntEnd{Hunt: h.start.Hunt, Result: result, Cores: slices.Clone(next.set), Masks: len(h.masks), Reason: "masked trial outcomes isolated the minimal failing set"}, Cause: []int{h.seq}}, true
	}
	profile := slices.Clone(h.start.Anchor)
	for _, id := range next.cores {
		profile[s.index(id)] = h.start.Failing[s.index(id)]
	}
	k := h.class.withDuration(next.duration)
	inferred := ""
	skipped := false
	reason := "testing the part of the failing profile"
	if s.passes(k, profile, h.seq) >= h.start.Starts {
		inferred = "pass"
		reason = "passing starts already establish the mask"
	} else if s.fails(k, profile, h.seq) {
		inferred = "failure"
		reason = "a known failure establishes the mask"
	} else if mark, ok := s.reaches(profile); ok {
		skipped = true
		reason = "its profile reaches " + mark
	}
	payload := s.makeMask(h, next, len(h.masks)+1, inferred, skipped, reason)
	payload.Profile = profile
	return Action{Kind: Decide, Payload: payload, Cause: []int{h.seq}}, true
}

func (s *State) makeMask(h *hunt, p maskPlan, number int, inferred string, skipped bool, reason string) *journal.HuntMask {
	profile := slices.Clone(h.start.Anchor)
	for _, id := range p.cores {
		profile[s.index(id)] = h.start.Failing[s.index(id)]
	}
	return &journal.HuntMask{Hunt: h.start.Hunt, Mask: number, Cores: slices.Clone(p.cores), Profile: profile, Set: slices.Clone(p.set), Granularity: p.g, Stage: p.stage, Index: p.index, DurationS: p.duration, Escalated: p.escalated, FullChecked: p.fullChecked, AnyFailed: p.anyFailed, Inferred: inferred, Skipped: skipped, Reason: reason}
}

func (s *State) huntCommitment(h *hunt) (Action, bool) {
	end := h.end
	if end.Result == "direct" {
		return Action{}, false
	}
	if len(end.Cores) == 1 {
		c := s.core(end.Cores[0])
		if c == nil {
			return Action{}, false
		}
		x := h.start.Failing[s.index(c.id)]
		mark := x
		if c.fail != nil {
			mark = max(mark, *c.fail)
		}
		to := max(c.offset, x+1)
		return Action{Kind: Decide, Payload: &journal.TunerDecision{Core: c.id, Phase: journal.PhaseHunt, Decision: journal.Backoff, FromOffset: c.offset, ToOffset: to, Pass: c.pass, FailedMark: new(mark), Reason: fmt.Sprintf("hunt %d found core %02d; failed mark %d", h.start.Hunt, c.id, mark)}, Cause: []int{h.endSeq}}, true
	}
	var marked *journal.JointMarkState
	for i := range s.marks {
		if s.marks[i].Hunt == h.start.Hunt {
			marked = &s.marks[i]
			break
		}
	}
	if marked == nil {
		members := make([]journal.JointMember, 0, len(end.Cores))
		for _, id := range end.Cores {
			members = append(members, journal.JointMember{Core: id, Offset: h.start.Failing[s.index(id)]})
		}
		reason := fmt.Sprintf("hunt %d found a combined failure", h.start.Hunt)
		for _, m := range members {
			if s.guard.profile[s.index(m.Core)] > m.Offset {
				reason = fmt.Sprintf("no backoff: core %02d is already shallower", m.Core)
				break
			}
		}
		return Action{Kind: Decide, Payload: &journal.MarkJoint{Mark: s.nextMark + 1, Members: members, Fallback: end.Result == "fallback", Hunt: h.start.Hunt, Reason: reason}, Cause: []int{h.endSeq}}, true
	}

	for _, m := range marked.Members {
		if s.guard.profile[s.index(m.Core)] > m.Offset {
			return Action{}, false
		}
	}
	rank := h.start.Ranking
	if len(rank) != len(s.cores) {
		rank = s.ids()
	}
	p := s.offsets()
	chosen := -1
	bestSum := int(^uint(0) >> 1)
	bestRank := -1
	for _, m := range marked.Members {
		c := s.core(m.Core)
		candidate := slices.Clone(p)
		i := s.index(m.Core)
		candidate[i] = max(candidate[i], m.Offset+1)
		target := s.optimum(candidate, candidate, rank)
		if target == nil {
			continue
		}
		total := 0
		for _, v := range target {
			total += v
		}
		order := slices.Index(rank, c.id)
		if total < bestSum || total == bestSum && order > bestRank {
			bestSum, chosen, bestRank = total, c.id, order
		}
	}
	if chosen < 0 {
		return Action{}, false
	}
	c := s.core(chosen)
	x := h.start.Failing[s.index(chosen)]
	to := max(c.offset, x+1)
	return Action{Kind: Decide, Payload: &journal.TunerDecision{Core: chosen, Phase: journal.PhaseHunt, Decision: journal.Backoff, FromOffset: c.offset, ToOffset: to, Pass: c.pass, FailedMark: c.fail, Reason: fmt.Sprintf("joint mark J%d: backing off core %02d leaves %d counts reachable", marked.Mark, chosen, -bestSum)}, Cause: []int{marked.Seq}}, true
}

func (s *State) projectHunt() *journal.HuntState {
	h := s.hunt
	if h == nil {
		return nil
	}
	p := h.start
	out := &journal.HuntState{Hunt: p.Hunt, Seq: h.seq, Failure: p.Failure, Regime: p.Regime, Trial: p.Trial, Anchor: slices.Clone(p.Anchor), AnchorSeq: p.AnchorSeq, Candidates: slices.Clone(p.Candidates)}
	for _, m := range h.masks {
		state := journal.MaskState{Mask: m.payload.Mask, Seq: m.seq, Cores: slices.Clone(m.payload.Cores), Outcome: s.maskOutcome(h, m), Needed: p.Starts}
		since := m.seq
		if m.payload.Inferred != "" {
			since = h.seq
		}
		state.Passes = s.passes(h.class.withDuration(m.payload.DurationS), m.payload.Profile, since)
		out.Masks = append(out.Masks, state)
		out.Escalated = m.payload.Escalated
	}
	return out
}
