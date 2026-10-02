package tuner

import (
	"fmt"
	"math"
	"slices"
	"strings"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type maskRecord struct {
	payload *journal.HuntMask
	seq     int
	cause   []int
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
		h.masks = append(h.masks, maskRecord{p, e.Seq, e.Cause})
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
		f := &s.pendingFailures[i]
		if f.carried {
			if _, valid := s.carriedSources[seq]; !valid {
				return nil
			}
		}
		return f
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
	for _, q := range slices.Backward(s.qualified) {
		if len(q.profile) != len(f.profile) {
			continue
		}
		raised := make([]int, len(q.profile))
		for i := range raised {
			raised[i] = max(q.profile[i], f.profile[i])
		}
		if slices.Equal(raised, f.profile) {
			continue
		}
		anchor = raised
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
	cause := []int{f.seq}
	if anchorSeq != 0 {
		cause = append(cause, anchorSeq)
	}
	return Action{Kind: Decide, Payload: p, Cause: cause}
}

func (s *State) split(h *hunt, set []int, g int) [][]int {
	if len(set) == 0 {
		return nil
	}
	g = max(1, min(g, len(set)))
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

func (s *State) repeatedMaskedCore(h *hunt, duration int) (int, [2]int, bool) {
	if len(s.recent) != 1 || len(h.start.Candidates) <= 2 {
		return 0, [2]int{}, false
	}
	id := s.recent[0]
	if !slices.Contains(h.start.Candidates, id) {
		return 0, [2]int{}, false
	}
	k := h.class.withDuration(duration)
	var newer *pendingFailure
	for i := len(s.pendingFailures) - 1; i >= 0; i-- {
		f := &s.pendingFailures[i]
		if !s.failureAfter(*f, s.resetSeq) {
			continue
		}
		if f.seq >= h.start.Failure || f.class != k || f.failure.Condition != machine.Masked || f.failure.Attribution != journal.Attributed || f.failure.Core == nil || f.failure.Offset == nil {
			continue
		}
		if *f.failure.Core != id {
			return 0, [2]int{}, false
		}
		if newer == nil {
			newer = f
			continue
		}
		if *newer.failure.Offset == *f.failure.Offset+1 && h.start.Failing[s.index(id)] == *newer.failure.Offset+1 {
			return id, [2]int{f.seq, newer.seq}, true
		}
		return 0, [2]int{}, false
	}
	return 0, [2]int{}, false
}

type maskPlan struct {
	set, cores                        []int
	g                                 int
	stage                             string
	index, duration                   int
	escalated, fullChecked, anyFailed bool
	edge                              *journal.JointMember
	held                              []journal.JointMember
	priority                          []int
	singleCorePrior                   [2]int
	result                            bool
}

func planOf(m *journal.HuntMask) maskPlan {
	return maskPlan{set: m.Set, cores: m.Cores, g: m.Granularity, stage: m.Stage, index: m.Index, duration: m.DurationS, escalated: m.Escalated, fullChecked: m.FullChecked, anyFailed: m.AnyFailed, edge: m.Edge, held: m.Held}
}

func (s *State) nextMaskPlan(h *hunt) (maskPlan, bool) {
	p := maskPlan{set: slices.Clone(h.start.Candidates), g: 2, stage: "part", duration: h.start.StartS}
	if len(h.masks) == 0 && h.start.Trial != "" && h.start.DurationS > h.start.StartS {
		shortFailed := slices.ContainsFunc(s.failures, func(e entry) bool {
			return (e.carried || e.seq > s.resetSeq) && e.seq < h.start.Failure && e.class.regime == h.class.regime && e.class.duration <= h.start.StartS
		})
		if !shortFailed {
			seqs := s.passSeqs(h.class.withDuration(h.start.StartS), h.start.Failing, s.resetSeq, huntEvidence)
			seqs = slices.DeleteFunc(seqs, func(seq int) bool { return seq >= h.start.Failure })
			if len(seqs) >= h.start.Starts {
				p.duration, p.escalated = h.start.DurationS, true
				p.priority = seqs[:h.start.Starts]
			}
		}
	}
	if len(p.set) == 0 {
		return p, false
	}
	split := slices.IndexFunc(h.masks, func(m maskRecord) bool { return m.payload.Edge != nil })
	if split < 0 {
		split = len(h.masks)
	}
	if split == 0 {
		if len(p.set) == 1 {
			p.result = true
			return p, false
		}
		if len(h.masks) == 0 {
			if id, prior, ok := s.repeatedMaskedCore(h, p.duration); ok {
				p.g, p.cores, p.singleCorePrior = len(p.set), []int{id}, prior
				return p, true
			}
		}
		p.cores = s.split(h, p.set, p.g)[0]
		return p, true
	}
	last := h.masks[split-1]
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
	if len(h.masks) == 1 && m.Stage == "part" && len(m.Cores) == 1 && m.Granularity > 2 && m.Granularity == len(h.start.Candidates) {
		p.g, p.index = 2, 0
		p.cores = s.split(h, p.set, p.g)[0]
		return p, true
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
	since := s.inferenceSince(m.payload, m.seq)
	if s.fails(k, m.payload.Profile, since) {
		return "failure"
	}
	if s.passes(k, m.payload.Profile, since, huntEvidence) >= h.start.Starts {
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
				return Action{Kind: Decide, Payload: s.makeMask(h, planOf(m.payload), m.payload.Mask, "", true, "its profile reaches "+mark), Cause: []int{m.seq}}, true
			}
			if s.retry != nil && s.retry.Hunt == h.start.Hunt && s.retry.Mask == m.payload.Mask {
				return Action{Kind: RunTrial, Trial: *s.retry, Cause: []int{m.seq}}, true
			}
			t := Trial{Regime: h.start.Regime, Workload: h.start.Workload, Condition: machine.Masked, Phase: journal.PhaseHunt, DurationS: m.payload.DurationS, Cores: slices.Clone(h.start.Cores), Profile: slices.Clone(m.payload.Profile), Hunt: h.start.Hunt, Mask: m.payload.Mask}
			return Action{Kind: RunTrial, Trial: t, Cause: []int{m.seq}}, true
		}
	}
	next, has := s.nextMaskPlan(h)
	if has {
		return s.planMask(h, next, "testing the part of the failing profile"), true
	}
	result, reason := "joint", "masked trial outcomes isolated the minimal failing set"
	var members []journal.JointMember
	switch {
	case len(next.set) == 1:
		result = "culprit"
	case !next.anyFailed:
		result, reason = "fallback", "no tested mask failed, so the remaining candidates stay unresolved and are marked together"
	default:
		probe, found, clause, probing := s.nextEdge(h, next)
		if probing {
			return s.planMask(h, probe, fmt.Sprintf("probing how shallow core %02d must be for the combination to pass", probe.edge.Core)), true
		}
		members, reason = found, reason+clause
	}
	cause := []int{h.seq}
	for _, m := range h.masks {
		cause = s.citeCarried(cause, m.cause...)
	}
	reason += s.carriedReason(cause)
	return Action{Kind: Decide, Payload: &journal.HuntEnd{Hunt: h.start.Hunt, Result: result, Cores: slices.Clone(next.set), Members: members, Masks: len(h.masks), Reason: reason}, Cause: cause}, true
}

func (s *State) planMask(h *hunt, p maskPlan, reason string) Action {
	payload := s.makeMask(h, p, len(h.masks)+1, "", false, reason)
	k := h.class.withDuration(p.duration)
	since := s.inferenceSince(payload, h.seq)
	cause := []int{h.seq}
	seqs := s.passSeqs(k, payload.Profile, since, huntEvidence)
	if len(seqs) >= h.start.Starts {
		seqs = seqs[:h.start.Starts]
		payload.Inferred, payload.Reason = "pass", "passing starts already establish the mask"+s.carriedReason(seqs)
		cause = s.citeCarried(cause, seqs...)
	} else if failure := s.failingSeq(k, payload.Profile, since); failure != 0 {
		payload.Inferred, payload.Reason = "failure", "a known failure establishes the mask"+s.carriedReason([]int{failure})
		cause = s.citeCarried(cause, failure)
	} else if mark, ok := s.reaches(payload.Profile); ok {
		payload.Skipped, payload.Reason = true, "its profile reaches "+mark
	}
	if len(p.priority) > 0 {
		payload.Reason += fmt.Sprintf("; %d valid short starts preceded the longer failure, with no failure in this regime at the short duration or less since reset, so test the failed duration", len(p.priority)) + s.carriedReason(p.priority)
		cause = append(cause, p.priority...)
	}
	if p.singleCorePrior[0] > 0 {
		payload.Reason += fmt.Sprintf("; two masked failures in this class followed one-count backoffs on core %02d, so probe that core first", p.cores[0]) + s.carriedReason(p.singleCorePrior[:])
		cause = append(cause, p.singleCorePrior[:]...)
	}
	return Action{Kind: Decide, Payload: payload, Cause: cause}
}

func (s *State) inferenceSince(m *journal.HuntMask, since int) int {
	if m.Stage == "part" || m.Stage == "complement" {
		return s.resetSeq
	}
	return since
}

func (s *State) makeMask(h *hunt, p maskPlan, number int, inferred string, skipped bool, reason string) *journal.HuntMask {
	profile := slices.Clone(h.start.Anchor)
	for _, id := range p.cores {
		profile[s.index(id)] = h.start.Failing[s.index(id)]
	}
	var edge *journal.JointMember
	if p.edge != nil {
		edge = new(*p.edge)
		for _, m := range p.held {
			profile[s.index(m.Core)] = m.Offset
		}
		profile[s.index(edge.Core)] = edge.Offset
	}
	return &journal.HuntMask{Hunt: h.start.Hunt, Mask: number, Cores: slices.Clone(p.cores), Profile: profile, Set: slices.Clone(p.set), Granularity: p.g, Stage: p.stage, Index: p.index, DurationS: p.duration, Escalated: p.escalated, FullChecked: p.fullChecked, AnyFailed: p.anyFailed, Edge: edge, Held: slices.Clone(p.held), Inferred: inferred, Skipped: skipped, Reason: reason}
}

func (s *State) nextEdge(h *hunt, joint maskPlan) (maskPlan, []journal.JointMember, string, bool) {
	members := make([]journal.JointMember, 0, len(joint.set))
	for _, id := range joint.set {
		i := s.index(id)
		if s.guard.profile[i] > h.start.Failing[i] {
			return maskPlan{}, nil, "", false
		}
		members = append(members, journal.JointMember{Core: id, Offset: h.start.Failing[i]})
	}
	probes := 0
	for k := range members {
		core := members[k].Core
		i := s.index(core)
		lo, hi := h.start.Failing[i], h.start.Anchor[i]
		failed, passed := 0, false
		for _, m := range h.masks {
			if m.payload.Edge == nil || m.payload.Edge.Core != core {
				continue
			}
			probes++
			switch s.maskOutcome(h, m) {
			case "failure":
				lo = max(lo, m.payload.Edge.Offset)
				failed++
			case "pass":
				hi = min(hi, m.payload.Edge.Offset)
				passed = true
			default:
				members[k].Offset = lo
				return maskPlan{}, members, "; edge probes stopped at a skipped mask with it failing at " + jointOffsets(members), false
			}
		}
		members[k].Offset = lo
		if hi-lo <= 1 {
			continue
		}
		probe := lo + (hi-lo)/2
		if gallop := h.start.Failing[i] + 1<<failed; !passed && gallop < hi {
			probe = gallop
		}
		held := slices.Delete(slices.Clone(members), k, k+1)
		cores := make([]int, len(held))
		for j, m := range held {
			cores[j] = m.Core
		}
		return maskPlan{set: slices.Clone(joint.set), cores: cores, g: joint.g, stage: "edge", index: probes, duration: joint.duration, escalated: joint.escalated, fullChecked: joint.fullChecked, anyFailed: true, edge: &journal.JointMember{Core: core, Offset: probe}, held: held}, nil, "", true
	}
	return maskPlan{}, members, "; edge probes found it still failing at " + jointOffsets(members) + " and passing with any one member a count shallower", false
}

func jointOffsets(members []journal.JointMember) string {
	parts := make([]string, len(members))
	for i, m := range members {
		parts[i] = fmt.Sprintf("core %02d %d", m.Core, m.Offset)
	}
	return strings.Join(parts, " + ")
}

func (s *State) huntRanking(h *hunt) []int {
	if len(h.start.Ranking) != len(s.cores) {
		return s.ids()
	}
	return h.start.Ranking
}

func (s *State) jointBackoff(members []journal.JointMember, rank []int) (journal.JointMember, int, bool) {
	p := s.offsets()
	var chosen journal.JointMember
	bestSum, bestRank, found := math.MaxInt, -1, false
	for _, m := range members {
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
		order := slices.Index(rank, m.Core)
		if total < bestSum || total == bestSum && order > bestRank {
			bestSum, chosen, bestRank, found = total, m, order, true
		}
	}
	return chosen, -bestSum, found
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
		pass, discarded := keepPass(c.pass, mark)
		return Action{Kind: Decide, Payload: &journal.TunerDecision{Core: c.id, Phase: journal.PhaseHunt, Decision: journal.Backoff, FromOffset: c.offset, ToOffset: to, Pass: pass, FailedMark: new(mark), Reason: fmt.Sprintf("hunt %d found core %02d; failed mark %d%s", h.start.Hunt, c.id, mark, discarded)}, Cause: []int{h.endSeq}}, true
	}
	var marked *journal.JointMarkState
	for i := range s.marks {
		if s.marks[i].Hunt == h.start.Hunt {
			marked = &s.marks[i]
			break
		}
	}
	if marked == nil {
		members := slices.Clone(end.Members)
		if members == nil {
			for _, id := range end.Cores {
				members = append(members, journal.JointMember{Core: id, Offset: h.start.Failing[s.index(id)]})
			}
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
	if c, probe, ok := s.testedBackoff(h); ok {
		to := probe.payload.Edge.Offset
		return Action{Kind: Decide, Payload: &journal.TunerDecision{Core: c.id, Phase: journal.PhaseHunt, Decision: journal.Backoff, FromOffset: c.offset, ToOffset: to, Pass: c.pass, FailedMark: c.fail, Reason: fmt.Sprintf("joint mark J%d: backing off core %02d to %d, where hunt %d mask %d passed with the rest of the joint at its failing offsets", marked.Mark, c.id, to, h.start.Hunt, probe.payload.Mask)}, Cause: []int{marked.Seq, probe.seq}}, true
	}
	chosen, reach, ok := s.jointBackoff(marked.Members, s.huntRanking(h))
	if !ok {
		return Action{}, false
	}
	c := s.core(chosen.Core)
	to := max(c.offset, chosen.Offset+1)
	return Action{Kind: Decide, Payload: &journal.TunerDecision{Core: c.id, Phase: journal.PhaseHunt, Decision: journal.Backoff, FromOffset: c.offset, ToOffset: to, Pass: c.pass, FailedMark: c.fail, Reason: fmt.Sprintf("joint mark J%d: backing off core %02d leaves %d counts reachable", marked.Mark, c.id, reach)}, Cause: []int{marked.Seq}}, true
}

func (s *State) testedBackoff(h *hunt) (*core, maskRecord, bool) {
	rank := s.huntRanking(h)
	var best *core
	var bestProbe maskRecord
	bestMove, bestOrder := 0, -1
	for _, m := range h.masks {
		edge := m.payload.Edge
		if edge == nil || s.maskOutcome(h, m) != "pass" {
			continue
		}
		alone := true
		for _, held := range m.payload.Held {
			if held.Offset != h.start.Failing[s.index(held.Core)] {
				alone = false
				break
			}
		}
		c := s.core(edge.Core)
		if !alone || c == nil || edge.Offset <= c.offset {
			continue
		}
		moved := s.offsets()
		moved[s.index(c.id)] = edge.Offset
		if _, reached := s.reaches(moved); reached {
			continue
		}
		move, order := edge.Offset-c.offset, slices.Index(rank, c.id)
		if best == nil || move < bestMove || move == bestMove && order > bestOrder {
			best, bestProbe, bestMove, bestOrder = c, m, move, order
		}
	}
	return best, bestProbe, best != nil
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
		if m.payload.Edge != nil {
			state.Edge = new(*m.payload.Edge)
			state.Held = slices.Clone(m.payload.Held)
		}
		since := m.seq
		if m.payload.Inferred != "" {
			since = h.seq
		}
		state.Passes = s.passes(h.class.withDuration(m.payload.DurationS), m.payload.Profile, s.inferenceSince(m.payload, since), huntEvidence)
		out.Masks = append(out.Masks, state)
		out.Escalated = m.payload.Escalated
	}
	return out
}
