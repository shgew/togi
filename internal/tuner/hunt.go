package tuner

import (
	"fmt"
	"math"
	"slices"
	"strings"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type groupRecord struct {
	payload *journal.HuntGroup
	seq     int
	cause   []int
}

type hunt struct {
	start                                 *journal.HuntStart
	seq                                   int
	class                                 trialClass
	groups                                []groupRecord
	end                                   *journal.HuntEnd
	endSeq, combinationSeq, directFailure int
}

func (s *State) openHunt(e journal.Event, p *journal.HuntStart) {
	s.hunt = &hunt{start: p, seq: e.Seq, class: trialClass{p.Regime, p.Workload, coresKey(p.Cores), p.DurationS}}
	s.nextHunt = max(s.nextHunt, p.Hunt)
	s.lastPlanSeq = e.Seq
	s.projectionDirty = true
	if s.hunt.located() {
		if s.located == nil {
			s.located = map[int]locatedHunt{}
		}
		s.located[p.Failure] = locatedHunt{hunt: p.Hunt}
		s.r7Epoch++
		return
	}
	if len(s.queue) > 0 && s.queue[0].seq == p.Failure {
		s.queue = s.queue[1:]
	}
}

func (s *State) recordGroup(e journal.Event, p *journal.HuntGroup) {
	if h := s.hunt; h != nil && p.Hunt == h.start.Hunt {
		m := groupRecord{p, e.Seq, e.Cause}
		if n := len(h.groups); n > 0 && h.groups[n-1].payload.Group == p.Group {
			h.groups[n-1] = m
		} else {
			h.groups = append(h.groups, m)
		}
		s.projectionDirty = true
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
	if h.located() {
		s.endLocatedHunt(h, e, p)
		return
	}
	if p.Result == "cancelled" {
		f := s.failureBySeq(h.start.Failure)
		if f != nil {
			s.queue = append([]pendingFailure{*f}, s.queue...)
		}
		s.hunt = nil
	}
}

func (s *State) resetHuntCore(c *core) {
	if c.queued == queuedReset && s.hunt != nil && s.hunt.end != nil && slices.Contains(s.hunt.end.Cores, c.id) {
		s.hunt = nil
	}
}

func (s *State) commitHuntDecision(e journal.Event, p *journal.TunerDecision) {
	if s.hunt != nil && s.hunt.end != nil && p.Phase == journal.PhaseHunt && len(e.Cause) > 0 && (e.Cause[0] == s.hunt.endSeq || e.Cause[0] == s.hunt.combinationSeq) {
		s.hunt = nil
	}
}

func (s *State) skipHunt(p *journal.HuntSkipped) {
	if len(s.queue) > 0 && s.queue[0].seq == p.Failure {
		s.queue = s.queue[1:]
	}
}

func (s *State) recordHuntCombination(e journal.Event, p *journal.Combination) {
	if s.hunt != nil && s.hunt.end != nil && s.hunt.start.Hunt == p.Hunt {
		s.hunt.combinationSeq = e.Seq
		if len(s.checking.profile) == len(s.cores) {
			for _, m := range p.Members {
				if s.checking.profile[s.index(m.Core)] > m.Offset {
					s.hunt = nil
					break
				}
			}
		}
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
	if reachedConstraint, ok := s.reaches(f.profile); ok {
		return Action{Kind: Decide, Payload: &journal.HuntSkipped{Failure: f.seq, Reason: "its profile reaches " + reachedConstraint}, Cause: []int{f.seq}}
	}
	parked := make([]int, len(f.profile))
	parkedSeq := 0
	for _, q := range slices.Backward(s.passedFullCycles) {
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
		parked = raised
		parkedSeq = q.seq
		break
	}
	var candidates []int
	for i, c := range s.byID() {
		if f.profile[i] < parked[i] {
			candidates = append(candidates, c.id)
		}
	}
	cores := slices.Clone(s.classTargets[f.class.cores].cores)
	if len(cores) == 0 {
		cores = s.ids()
	}
	p := &journal.HuntStart{Hunt: s.nextHunt + 1, Failure: f.seq, Trial: f.failure.Trial, Regime: f.class.regime, Workload: f.class.workload, Cores: cores, DurationS: f.class.duration, Failing: slices.Clone(f.profile), Parked: parked, ParkedSeq: parkedSeq, Candidates: candidates, Trials: s.n, TrialS: s.durations.ShortTrialS, Miss: s.evidence.Miss, Rate: s.evidence.Rate, Ranking: slices.Clone(s.ranking)}
	p.Reason = f.failure.Reason
	cause := []int{f.seq}
	if parkedSeq != 0 {
		cause = append(cause, parkedSeq)
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

func (s *State) repeatedParkedCore(h *hunt, duration int) (int, [2]int, bool) {
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
		if f.seq >= h.start.Failure || f.class != k || f.failure.Condition != machine.Parked || f.failure.Attribution != journal.Attributed || f.failure.Core == nil || f.failure.Offset == nil {
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

type groupPlan struct {
	set, cores                        []int
	g                                 int
	stage                             string
	index, duration                   int
	escalated, fullChecked, anyFailed bool
	probe                             *journal.CombinationMember
	held                              []journal.CombinationMember
	priority                          []int
	singleCorePrior                   [2]int
	result, loaded                    bool
}

func planOf(m *journal.HuntGroup) groupPlan {
	return groupPlan{set: m.Set, cores: m.Cores, g: m.Granularity, stage: m.Stage, index: m.Index, duration: m.DurationS, escalated: m.Escalated, fullChecked: m.FullChecked, anyFailed: m.AnyFailed, probe: m.Probe, held: m.Held}
}

func (s *State) nextGroupPlan(h *hunt) (groupPlan, bool) {
	if p, has, decided := s.locatePlan(h); decided {
		return p, has
	}
	groups := h.narrowing()
	p := groupPlan{set: slices.Clone(h.start.Candidates), g: 2, stage: "part", duration: h.start.TrialS}
	if len(groups) == 0 && h.start.Trial != "" && h.start.DurationS > h.start.TrialS {
		shortFailed := slices.ContainsFunc(s.failures, func(e entry) bool {
			return (e.carried || e.seq > s.resetSeq) && e.seq < h.start.Failure && e.class.regime == h.class.regime && e.class.duration <= h.start.TrialS
		})
		if !shortFailed {
			seqs := s.passSeqs(h.class.withDuration(h.start.TrialS), h.start.Failing, s.resetSeq, huntEvidence)
			seqs = slices.DeleteFunc(seqs, func(seq int) bool { return seq >= h.start.Failure })
			if len(seqs) >= h.start.Trials {
				p.duration, p.escalated = h.start.DurationS, true
				p.priority = seqs[:h.start.Trials]
			}
		}
	}
	if len(p.set) == 0 {
		return p, false
	}
	split := slices.IndexFunc(groups, func(m groupRecord) bool { return m.payload.Probe != nil })
	if split < 0 {
		split = len(groups)
	}
	if split == 0 {
		if len(p.set) == 1 {
			p.result = true
			return p, false
		}
		if len(groups) == 0 {
			if id, prior, ok := s.repeatedParkedCore(h, p.duration); ok {
				p.g, p.cores, p.singleCorePrior = len(p.set), []int{id}, prior
				return p, true
			}
		}
		p.cores = s.split(h, p.set, p.g)[0]
		return p, true
	}
	last := groups[split-1]
	m := last.payload
	p = groupPlan{set: slices.Clone(m.Set), g: m.Granularity, stage: m.Stage, index: m.Index, duration: m.DurationS, escalated: m.Escalated, fullChecked: m.FullChecked, anyFailed: m.AnyFailed}
	outcome := s.groupOutcome(h, last)
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
	if len(groups) == 1 && m.Stage == "part" && len(m.Cores) == 1 && m.Granularity > 2 && m.Granularity == len(h.start.Candidates) {
		p.g, p.index = 2, 0
		p.cores = s.split(h, p.set, p.g)[0]
		return p, true
	}
	if p.stage == "full" {
		p.fullChecked = true
		if outcome == "pass" {
			return s.afterPassedFull(h, p)
		}
	}
	if p.stage == "part" || p.stage == "complement" {
		parts := s.split(h, p.set, p.g)
		if p.index+1 < len(parts) {
			p.index++
			p.cores = s.groupPart(parts, p.stage, p.index, p.set)
			return p, true
		}
		if p.stage == "part" && p.g > 2 {
			p.stage = "complement"
			p.index = 0
			p.cores = s.groupPart(parts, p.stage, p.index, p.set)
			return p, true
		}
	}
	if !p.escalated && !p.fullChecked && p.g == 2 && slices.Equal(p.set, h.start.Candidates) && h.start.DurationS > h.start.TrialS {
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
	return h.lastGroupPlan(p)
}

// afterPassedFull plans the step after a passed full group: a located hunt's full group at the failed duration
// ends it; any other escalates the narrowing to the failed duration.
func (s *State) afterPassedFull(h *hunt, p groupPlan) (groupPlan, bool) {
	if h.locatedFull(p) {
		p.result = true
		return p, false
	}
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

// lastGroupPlan ends narrowing, unless a located hunt with no failed group still needs its full failing profile at
// the failed duration.
func (h *hunt) lastGroupPlan(p groupPlan) (groupPlan, bool) {
	if h.located() && !p.anyFailed && !h.locatedFull(p) {
		p.stage, p.index, p.duration = "full", 0, h.start.DurationS
		p.escalated = p.escalated || h.start.DurationS > h.start.TrialS
		p.cores = slices.Clone(p.set)
		return p, true
	}
	p.result = true
	return p, false
}

func (s *State) groupPart(parts [][]int, stage string, index int, set []int) []int {
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

func (s *State) groupEvidence(h *hunt, m groupRecord, cite bool) (string, int, []int) {
	if m.payload.Skipped {
		return "skipped", 0, nil
	}
	if m.payload.Inferred != "" {
		return m.payload.Inferred, 0, nil
	}
	k := h.class.withDuration(m.payload.DurationS)
	since := s.inferenceSince(m.payload, m.seq)
	if failure := s.failingSeq(k, m.payload.Profile, since); failure != 0 {
		return "failure", failure, nil
	}
	var seqs []int
	count := 0
	if cite {
		seqs = s.passSeqs(k, m.payload.Profile, since, huntEvidence)
		count = len(seqs)
	} else {
		count = s.passes(k, m.payload.Profile, since, huntEvidence)
	}
	if count >= h.start.Trials {
		if cite {
			seqs = seqs[:h.start.Trials]
		}
		return "pass", 0, seqs
	}
	return "running", 0, nil
}

func (s *State) groupOutcome(h *hunt, m groupRecord) string {
	outcome, _, _ := s.groupEvidence(h, m, false)
	return outcome
}

// citeGroup adds what established a group's outcome: the failure that rejected it, live or carried, or, for a group
// whose failure was inferred, the hunt.group recording that inference and citing its failure. Of passing evidence, only
// carried passes are cited. A settled group is cited as it stood when the next group was recorded.
func (s *State) citeGroup(cause []int, h *hunt, i int) []int {
	m := h.groups[i]
	cause = s.citeCarried(cause, m.cause...)
	if m.payload.Inferred == "failure" {
		return citeNew(cause, m.seq)
	}
	if h.settled(i) && !m.payload.Skipped && m.payload.Inferred == "" {
		k := h.class.withDuration(m.payload.DurationS)
		since, until := s.inferenceSince(m.payload, m.seq), h.groups[i+1].seq
		if failure := s.failingSeqBefore(k, m.payload.Profile, since, until); failure != 0 {
			return citeNew(cause, failure)
		}
		seqs := s.passSeqsBefore(k, m.payload.Profile, since, until, huntEvidence)
		return s.citeCarried(cause, seqs[:min(len(seqs), h.start.Trials)]...)
	}
	_, failure, seqs := s.groupEvidence(h, m, true)
	if failure == 0 {
		return s.citeCarried(cause, seqs...)
	}
	if i+1 < len(h.groups) {
		// A later group's failure at a shallower profile also rejects this one; cite the failure seen before the hunt moved on.
		k := h.class.withDuration(m.payload.DurationS)
		if before := s.failingSeqBefore(k, m.payload.Profile, s.inferenceSince(m.payload, m.seq), h.groups[i+1].seq); before != 0 {
			failure = before
		}
	}
	return citeNew(cause, failure)
}

// settled reports a group whose outcome no later decision reads again: a narrowing group the hunt followed with another.
// The hunt's latest group, a located hunt's locate group, the narrowing group before the first member probe and every probe are
// read again, so a failure recorded at their profile after the hunt moved on can decide them.
func (h *hunt) settled(i int) bool {
	m := h.groups[i].payload
	return i+1 < len(h.groups) && m.Probe == nil && m.Stage != "locate" && h.groups[i+1].payload.Probe == nil
}

func (s *State) huntNext() (Action, bool) {
	h := s.hunt
	if h.end != nil {
		return s.huntCommitment(h)
	}
	if len(h.groups) > 0 {
		m := h.groups[len(h.groups)-1]
		if s.groupOutcome(h, m) == "running" {
			if reachedConstraint, ok := s.reaches(m.payload.Profile); ok {
				return Action{Kind: Decide, Payload: s.makeGroup(h, planOf(m.payload), m.payload.Group, "", true, "its profile reaches "+reachedConstraint), Cause: []int{m.seq}}, true
			}
			if s.retry != nil && s.retry.Hunt == h.start.Hunt && s.retry.Group == m.payload.Group {
				return Action{Kind: RunTrial, Trial: *s.retry, Cause: []int{m.seq}}, true
			}
			t := Trial{Regime: h.start.Regime, Workload: h.start.Workload, Condition: machine.Parked, Phase: journal.PhaseHunt, DurationS: m.payload.DurationS, Cores: slices.Clone(h.start.Cores), Profile: slices.Clone(m.payload.Profile), Hunt: h.start.Hunt, Group: m.payload.Group}
			return Action{Kind: RunTrial, Trial: t, Cause: []int{m.seq}}, true
		}
	}
	if a, ok := s.locatedLoadedFailure(h); ok {
		return a, true
	}
	next, has := s.nextGroupPlan(h)
	if has {
		reason := "testing the part of the failing profile"
		switch {
		case next.stage == "locate":
			reason = "rerunning the failed load with every unloaded core at CO 0 to locate the failure"
		case h.locatedFull(next):
			reason = "rerunning the full failing profile at the failed duration before the failure stays with the loaded cores"
		}
		return s.planGroup(h, next, reason), true
	}
	result, reason := "combination", "parked trial outcomes identified the minimal failing set"
	var members []journal.CombinationMember
	cores := slices.Clone(next.set)
	switch {
	case next.loaded:
		result, reason, cores = "loaded", "the failed load failed again with every unloaded core at CO 0, so the failure stays with the loaded cores", slices.Clone(h.start.Cores)
	case len(next.set) == 1:
		result = "culprit"
	case !next.anyFailed && h.located() && slices.ContainsFunc(h.start.Cores, func(id int) bool { return !s.atCOZero(h.start.Failing, id) }):
		result, reason, cores = "loaded", "the full failing profile passed while narrowing the unloaded cores, so the failure stays with the loaded cores", slices.Clone(h.start.Cores)
	case !next.anyFailed:
		result, reason = "fallback", "no tested group failed, so the remaining candidates stay unresolved and form a combination"
	default:
		probe, found, clause, probing := s.nextMemberProbe(h, next)
		if probing {
			return s.planGroup(h, probe, fmt.Sprintf("probing how shallow core %02d must be for the combination to pass", probe.probe.Core)), true
		}
		members, reason = found, reason+clause
	}
	cause := []int{h.seq}
	if result == "loaded" {
		if failure := s.locateFailure(h); failure != 0 {
			cause = append(cause, failure)
		}
	}
	for i := range h.groups {
		cause = s.citeGroup(cause, h, i)
	}
	reason += s.carriedReason(cause)
	return Action{Kind: Decide, Payload: &journal.HuntEnd{Hunt: h.start.Hunt, Result: result, Cores: cores, Members: members, Groups: len(h.groups), Reason: reason}, Cause: cause}, true
}

func (s *State) planGroup(h *hunt, p groupPlan, reason string) Action {
	payload := s.makeGroup(h, p, len(h.groups)+1, "", false, reason)
	k := h.class.withDuration(p.duration)
	since := s.inferenceSince(payload, h.seq)
	cause := []int{h.seq}
	seqs := s.passSeqs(k, payload.Profile, since, huntEvidence)
	if len(seqs) >= h.start.Trials {
		seqs = seqs[:h.start.Trials]
		payload.Inferred, payload.Reason = "pass", "passing trials already establish the group"
		cause = s.citeCarried(cause, seqs...)
	} else if failure := s.failingSeq(k, payload.Profile, since); failure != 0 {
		payload.Inferred, payload.Reason = "failure", "a known failure establishes the group"
		cause = citeNew(cause, failure)
	} else if reachedConstraint, ok := s.reaches(payload.Profile); ok {
		payload.Skipped, payload.Reason = true, "its profile reaches "+reachedConstraint
	}
	if len(p.priority) > 0 {
		payload.Reason += fmt.Sprintf("; %d valid short trials preceded the longer failure, with no failure in this regime at the short duration or less since reset, so test the failed duration", len(p.priority))
		cause = append(cause, p.priority...)
	}
	if p.singleCorePrior[0] > 0 {
		payload.Reason += fmt.Sprintf("; two parked failures in this class followed one-count backoffs on core %02d, so probe that core first", p.cores[0])
		cause = append(cause, p.singleCorePrior[:]...)
	}
	for i := range h.groups {
		cause = s.citeGroup(cause, h, i)
	}
	payload.Reason += s.carriedReason(cause)
	return Action{Kind: Decide, Payload: payload, Cause: cause}
}

func (s *State) inferenceSince(m *journal.HuntGroup, since int) int {
	if m.Stage == "part" || m.Stage == "complement" {
		return s.resetSeq
	}
	return since
}

func (s *State) makeGroup(h *hunt, p groupPlan, number int, inferred string, skipped bool, reason string) *journal.HuntGroup {
	profile := slices.Clone(h.start.Parked)
	for _, id := range p.cores {
		profile[s.index(id)] = h.start.Failing[s.index(id)]
	}
	var probe *journal.CombinationMember
	if p.probe != nil {
		probe = new(*p.probe)
		for _, m := range p.held {
			profile[s.index(m.Core)] = m.Offset
		}
		profile[s.index(probe.Core)] = probe.Offset
	}
	return &journal.HuntGroup{Hunt: h.start.Hunt, Group: number, Cores: slices.Clone(p.cores), Profile: profile, Set: slices.Clone(p.set), Granularity: p.g, Stage: p.stage, Index: p.index, DurationS: p.duration, Escalated: p.escalated, FullChecked: p.fullChecked, AnyFailed: p.anyFailed, Probe: probe, Held: slices.Clone(p.held), Inferred: inferred, Skipped: skipped, Reason: reason}
}

func (s *State) nextMemberProbe(h *hunt, combination groupPlan) (groupPlan, []journal.CombinationMember, string, bool) {
	members := make([]journal.CombinationMember, 0, len(combination.set))
	for _, id := range combination.set {
		i := s.index(id)
		if s.checking.profile[i] > h.start.Failing[i] {
			return groupPlan{}, nil, "", false
		}
		members = append(members, journal.CombinationMember{Core: id, Offset: h.start.Failing[i]})
	}
	probes := 0
	for k := range members {
		core := members[k].Core
		i := s.index(core)
		lo, hi := h.start.Failing[i], h.start.Parked[i]
		failed, passed := 0, false
		for _, m := range h.groups {
			if m.payload.Probe == nil || m.payload.Probe.Core != core {
				continue
			}
			probes++
			switch s.groupOutcome(h, m) {
			case "failure":
				lo = max(lo, m.payload.Probe.Offset)
				failed++
			case "pass":
				hi = min(hi, m.payload.Probe.Offset)
				passed = true
			default:
				members[k].Offset = lo
				return groupPlan{}, members, "; member probes stopped at a skipped group with it failing at " + combinationOffsets(members), false
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
		return groupPlan{set: slices.Clone(combination.set), cores: cores, g: combination.g, stage: "probe", index: probes, duration: combination.duration, escalated: combination.escalated, fullChecked: combination.fullChecked, anyFailed: true, probe: &journal.CombinationMember{Core: core, Offset: probe}, held: held}, nil, "", true
	}
	return groupPlan{}, members, "; member probes found it still failing at " + combinationOffsets(members) + " and passing with any one member a count shallower", false
}

func combinationOffsets(members []journal.CombinationMember) string {
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

func (s *State) combinationBackoff(members []journal.CombinationMember, rank []int) (journal.CombinationMember, int, bool) {
	p := s.offsets()
	var chosen journal.CombinationMember
	bestSum, bestRank, found := math.MaxInt, -1, false
	for _, m := range members {
		candidate := slices.Clone(p)
		i := s.index(m.Core)
		candidate[i] = max(candidate[i], m.Offset+1)
		target := s.optimum(candidate, rank)
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
		failurePoint := x
		if c.fail != nil {
			failurePoint = max(failurePoint, *c.fail)
		}
		to := max(c.offset, x+1)
		pass, discarded := keepPass(c.pass, failurePoint)
		return Action{Kind: Decide, Payload: &journal.TunerDecision{Core: c.id, Phase: journal.PhaseHunt, Decision: journal.Backoff, FromOffset: c.offset, ToOffset: to, Pass: pass, FailurePoint: new(failurePoint), Reason: fmt.Sprintf("hunt %d found core %02d; failure point %d%s", h.start.Hunt, c.id, failurePoint, discarded)}, Cause: []int{h.endSeq}}, true
	}
	var recordedCombination *journal.CombinationState
	for i := range s.combinations {
		if s.combinations[i].Hunt == h.start.Hunt {
			recordedCombination = &s.combinations[i]
			break
		}
	}
	if recordedCombination == nil {
		members := slices.Clone(end.Members)
		if members == nil {
			for _, id := range end.Cores {
				members = append(members, journal.CombinationMember{Core: id, Offset: h.start.Failing[s.index(id)]})
			}
		}
		reason := fmt.Sprintf("hunt %d found a combined failure", h.start.Hunt)
		for _, m := range members {
			if s.checking.profile[s.index(m.Core)] > m.Offset {
				reason = fmt.Sprintf("no backoff: core %02d is already shallower", m.Core)
				break
			}
		}
		return Action{Kind: Decide, Payload: &journal.Combination{Combination: s.nextCombination + 1, Members: members, Fallback: end.Result == "fallback", Hunt: h.start.Hunt, Reason: reason}, Cause: []int{h.endSeq}}, true
	}

	for _, m := range recordedCombination.Members {
		if s.checking.profile[s.index(m.Core)] > m.Offset {
			return Action{}, false
		}
	}
	if c, probe, ok := s.testedBackoff(h); ok {
		to := probe.payload.Probe.Offset
		return Action{Kind: Decide, Payload: &journal.TunerDecision{Core: c.id, Phase: journal.PhaseHunt, Decision: journal.Backoff, FromOffset: c.offset, ToOffset: to, Pass: c.pass, FailurePoint: c.fail, Reason: fmt.Sprintf("combination C%d: backing off core %02d to %d, where hunt %d group %d passed with the rest of the combination at its failing offsets", recordedCombination.Combination, c.id, to, h.start.Hunt, probe.payload.Group)}, Cause: []int{recordedCombination.Seq, probe.seq}}, true
	}
	chosen, reach, ok := s.combinationBackoff(recordedCombination.Members, s.huntRanking(h))
	if !ok {
		return Action{}, false
	}
	c := s.core(chosen.Core)
	to := max(c.offset, chosen.Offset+1)
	return Action{Kind: Decide, Payload: &journal.TunerDecision{Core: c.id, Phase: journal.PhaseHunt, Decision: journal.Backoff, FromOffset: c.offset, ToOffset: to, Pass: c.pass, FailurePoint: c.fail, Reason: fmt.Sprintf("combination C%d: backing off core %02d leaves %d counts reachable", recordedCombination.Combination, c.id, reach)}, Cause: []int{recordedCombination.Seq}}, true
}

func (s *State) testedBackoff(h *hunt) (*core, groupRecord, bool) {
	rank := s.huntRanking(h)
	var best *core
	var bestProbe groupRecord
	bestMove, bestOrder := 0, -1
	for _, m := range h.groups {
		probe := m.payload.Probe
		if probe == nil || s.groupOutcome(h, m) != "pass" {
			continue
		}
		alone := true
		for _, held := range m.payload.Held {
			if held.Offset != h.start.Failing[s.index(held.Core)] {
				alone = false
				break
			}
		}
		c := s.core(probe.Core)
		if !alone || c == nil || probe.Offset <= c.offset {
			continue
		}
		moved := s.offsets()
		moved[s.index(c.id)] = probe.Offset
		if _, reached := s.reaches(moved); reached {
			continue
		}
		move, order := probe.Offset-c.offset, slices.Index(rank, c.id)
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
	if !s.projectionDirty && s.projectedHunt != nil {
		return s.projectedHunt
	}
	p := h.start
	out := &journal.HuntState{Hunt: p.Hunt, Seq: h.seq, Failure: p.Failure, Regime: p.Regime, Trial: p.Trial, Parked: slices.Clone(p.Parked), ParkedSeq: p.ParkedSeq, Candidates: slices.Clone(p.Candidates)}
	for _, m := range h.groups {
		state := journal.GroupState{Group: m.payload.Group, Seq: m.seq, Cores: slices.Clone(m.payload.Cores), Outcome: s.groupOutcome(h, m), Needed: p.Trials}
		if m.payload.Probe != nil {
			state.Probe = new(*m.payload.Probe)
			state.Held = slices.Clone(m.payload.Held)
		}
		since := m.seq
		if m.payload.Inferred != "" {
			since = h.seq
		}
		state.Passes = s.passes(h.class.withDuration(m.payload.DurationS), m.payload.Profile, s.inferenceSince(m.payload, since), huntEvidence)
		out.Groups = append(out.Groups, state)
		out.Escalated = m.payload.Escalated
	}
	s.projectedHunt = out
	return out
}
