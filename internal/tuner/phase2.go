package tuner

import (
	"fmt"
	"slices"
	"strings"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/requests"
)

type charge struct {
	seq   int
	off   int
	alone bool
	r7    bool
	top   bool
}

type phase2State struct {
	started     bool
	confirming  bool
	concluded   bool
	concludeSeq int
	startSeq    int
	conf        map[int]int
	candidates  map[int]bool
	literal     map[int]bool
	done        map[int]bool
	consumed    map[int]bool
	lastRound   *round
}

func p2Tag() string {
	return fmt.Sprintf("phase 2 (experiment phase2=%s, tol=%d)", exp.Phase2, exp.Tol)
}

func (s *State) p2Active() bool {
	return exp.Phase2 != "" && s.p2.started && !s.p2.concluded
}

func (s *State) foldCharge(e journal.Event, p *journal.TunerDecision) {
	if p.Decision != journal.Backoff || p.Phase == journal.PhaseSearch {
		return
	}
	c := s.core(p.Core)
	if c == nil {
		return
	}
	for _, seq := range e.Cause {
		if slices.ContainsFunc(s.combinations, func(m journal.CombinationState) bool { return m.Seq == seq }) {
			return
		}
	}
	ch := charge{seq: e.Seq, off: p.FromOffset}
	for _, seq := range e.Cause {
		f := s.failureBySeq(seq)
		if f == nil {
			continue
		}
		if intent := s.intents[f.failure.Trial]; intent != nil && intent.Condition == machine.Alone {
			ch.alone = true
		}
		if s.multiR7(f.class) {
			ch.r7 = true
			if failed := s.r7FailureEntry(*f); failed != nil {
				ch.top = slices.Contains(s.entryTop(*failed), p.Core)
			}
		}
		break
	}
	c.charges = append(c.charges, ch)
}

func (s *State) foldCombinationCharges(e journal.Event, p *journal.Combination) {
	for _, m := range p.Members {
		if c := s.core(m.Core); c != nil {
			c.charges = append(c.charges, charge{seq: e.Seq, off: m.Offset})
		}
	}
}

func (s *State) p2Evaluate(profile []int) (cands, literal map[int]bool) {
	cands, literal = map[int]bool{}, map[int]bool{}
	for _, c := range s.cores {
		p := profile[s.index(c.id)]
		if p-c.ceiling <= exp.Tol {
			continue
		}
		total, loose := 0, 0
		blocked := false
		for _, ch := range c.charges {
			switch {
			case ch.alone && ch.off >= c.ceiling && ch.off < p:
				blocked = true
			case !ch.alone && ch.off < p:
				total++
				if ch.r7 && !ch.top {
					loose++
				}
			}
		}
		if blocked {
			continue
		}
		if total <= 1 {
			cands[c.id] = true
		}
		if total == 0 || total == 1 && loose == 1 {
			literal[c.id] = true
		}
	}
	return
}

func ids(m map[int]bool) []int {
	out := make([]int, 0, len(m))
	for id := range m {
		out = append(out, id)
	}
	slices.Sort(out)
	return out
}

func (s *State) p2CycleStartReason() string {
	if !s.p2.started || s.p2.confirming {
		return ""
	}
	return p2Tag() + ": confirmation cycle after the last round"
}

func (s *State) p2CycleEndReason() string {
	if exp.Phase2 == "" {
		return ""
	}
	switch {
	case !s.p2.started:
		cands, literal := s.p2Evaluate(s.checking.profile)
		_, _, movable := s.p2Targets(cands)
		if len(movable) == 0 {
			return fmt.Sprintf("%s: phase 1 first passed cycle, candidate cores %v (the literal Q26 clause would allow %v), none can move without reaching a failure point or combination, the run concludes", p2Tag(), ids(cands), ids(literal))
		}
		return fmt.Sprintf("%s: phase 1 first passed cycle, candidate cores %v (the literal Q26 clause would allow %v), movable now %v, rounds follow", p2Tag(), ids(cands), ids(literal), movable)
	case s.p2.confirming && !s.p2.concluded:
		return p2Tag() + ": confirmation cycle passed, the run concludes"
	}
	return ""
}

func (s *State) foldPhase2Cycle(e journal.Event, p *journal.CheckingCycle) {
	if exp.Phase2 == "" {
		return
	}
	q := &s.p2
	if p.Event == journal.CycleStart {
		if q.started {
			q.confirming = true
		}
		return
	}
	if !p.Passed || !p.Full {
		return
	}
	if !q.started {
		q.started, q.startSeq = true, e.Seq
		q.conf = map[int]int{}
		for _, c := range s.cores {
			q.conf[c.id] = s.checking.profile[s.index(c.id)]
		}
		q.candidates, q.literal = s.p2Evaluate(s.checking.profile)
		q.done, q.consumed = map[int]bool{}, map[int]bool{}
		if _, _, movable := s.p2Targets(q.candidates); len(movable) == 0 {
			q.confirming, q.concluded, q.concludeSeq = true, true, e.Seq
		}
		return
	}
	if q.confirming && !q.concluded {
		q.concluded, q.concludeSeq = true, e.Seq
	}
}

func (s *State) p2Targets(cands map[int]bool) (profile, target, changed []int) {
	cur := s.offsets()
	profile = slices.Clone(cur)
	target = slices.Clone(cur)
	for _, c := range s.cores {
		if !cands[c.id] || s.p2.done[c.id] || c.phase == journal.PhaseSearch || c.offset <= c.ceiling {
			continue
		}
		step := 1
		if exp.Phase2 == "b" {
			step = (c.offset - c.ceiling + 1) / 2
		}
		t := max(c.offset-step, c.ceiling)
		if c.fail != nil && t <= *c.fail {
			continue
		}
		q := slices.Clone(profile)
		q[s.index(c.id)] = t
		if _, hit := s.reaches(q); hit {
			continue
		}
		profile = q
		target[s.index(c.id)] = c.ceiling
		changed = append(changed, c.id)
	}
	return
}

func (s *State) p2RoundDue() bool {
	q := &s.p2
	if exp.Phase2 == "" || !q.started || q.confirming || q.concluded || s.checking.open || s.round != nil || s.hunt != nil || len(s.queue) > 0 || len(s.obligations) > 0 || s.anySearch() {
		return false
	}
	_, _, changed := s.p2Targets(q.candidates)
	return len(changed) > 0
}

func (s *State) p2RoundStart() Action {
	profile, target, changed := s.p2Targets(s.p2.candidates)
	base := s.passedFullCycles[len(s.passedFullCycles)-1]
	reason := fmt.Sprintf("; %s: round moves cores %v toward their ceilings", p2Tag(), changed)
	if s.nextRound == 0 {
		reason += fmt.Sprintf("; %d candidate cores %v, the literal Q26 clause would allow %d (%v)", len(s.p2.candidates), ids(s.p2.candidates), len(s.p2.literal), ids(s.p2.literal))
	}
	return Action{Kind: Decide, Payload: &journal.DeepeningRound{Round: s.nextRound + 1, Event: journal.CycleStart, Base: slices.Clone(base.profile), BaseSeq: base.seq, Target: target, Profile: profile, Cores: changed, Ranking: slices.Clone(s.ranking), Trials: s.n, TrialS: s.durations.ShortTrialS, Reason: reason}, Cause: []int{base.seq}}
}

func (s *State) p2MoveReason(round, target int) string {
	return fmt.Sprintf("%s: round %d moves toward its ceiling %d", p2Tag(), round, target)
}

func (s *State) p2Consume(e journal.Event, p *journal.TunerDecision) {
	if exp.Phase2 == "" || !strings.HasPrefix(p.Reason, p2Tag()) || p.Decision != journal.Backoff {
		return
	}
	q := &s.p2
	q.done[p.Core] = true
	for _, seq := range e.Cause {
		q.consumed[seq] = true
		s.queue = slices.DeleteFunc(s.queue, func(f pendingFailure) bool { return f.seq == seq })
		if s.r7Handled == nil {
			s.r7Handled = map[int]map[int]bool{}
		}
		if s.r7Handled[seq] == nil {
			s.r7Handled[seq] = map[int]bool{}
		}
		for _, c := range s.cores {
			s.r7Handled[seq][c.id] = true
			if c.pending == seq {
				c.pending = 0
			}
		}
	}
}

type p2Failure struct {
	f        pendingFailure
	intent   *journal.TrialIntent
	rd       *round
	loaded   []int
	deepened []int
	profile  []int
}

func (s *State) p2Find() (p2Failure, bool) {
	if !s.p2Active() {
		return p2Failure{}, false
	}
	q := &s.p2
	for _, f := range s.pendingFailures {
		if f.seq <= q.startSeq || f.carried || f.failure.KnownFailure != 0 || q.consumed[f.seq] {
			continue
		}
		out := p2Failure{f: f, intent: s.intents[f.failure.Trial]}
		for _, rd := range []*round{s.round, q.lastRound} {
			if rd != nil && out.intent != nil && out.intent.Round > 0 && out.intent.Round == rd.start.Round {
				out.rd = rd
				break
			}
		}
		if out.rd == nil && !q.confirming {
			continue
		}
		out.profile = f.profile
		if len(out.profile) != len(s.cores) && out.intent != nil {
			out.profile = out.intent.Profile
		}
		switch {
		case out.intent == nil:
			out.loaded = s.ids()
		case out.intent.Core != nil:
			out.loaded = []int{*out.intent.Core}
		case len(out.intent.Cores) > 0:
			out.loaded = out.intent.Cores
		default:
			out.loaded = s.ids()
		}
		for _, id := range out.loaded {
			i := s.index(id)
			if out.rd != nil {
				if slices.Contains(out.rd.start.Cores, id) && out.rd.start.Profile[i] < out.rd.initial[i] {
					out.deepened = append(out.deepened, id)
				}
			} else if len(out.profile) == len(s.cores) && out.profile[i] < q.conf[id] {
				out.deepened = append(out.deepened, id)
			}
		}
		if len(out.deepened) > 0 {
			return out, true
		}
	}
	return p2Failure{}, false
}

func (s *State) p2Blamed(x p2Failure) int {
	d := x.deepened
	if f := x.f.failure; f.Attribution == journal.Attributed && f.Core != nil && slices.Contains(d, *f.Core) {
		return *f.Core
	}
	if s.multiR7(x.f.class) {
		if failed := s.r7FailureEntry(x.f); failed != nil {
			if failed.named != nil && slices.Contains(d, *failed.named) {
				return *failed.named
			}
			req, _ := s.trialRequests(*failed)
			part := map[int]float64{}
			for _, id := range d {
				if v, ok := req[id]; ok {
					part[id] = v
				}
			}
			if groups := requests.Groups(part); len(groups) > 0 {
				return s.p2Pick(groups[0])
			}
		}
	}
	depth := func(id int) int {
		if x.rd != nil {
			return x.rd.start.Profile[s.index(id)] - x.rd.initial[s.index(id)]
		}
		return x.profile[s.index(id)] - s.p2.conf[id]
	}
	best := d[:1]
	for _, id := range d[1:] {
		switch {
		case depth(id) < depth(best[0]):
			best = []int{id}
		case depth(id) == depth(best[0]):
			best = append(slices.Clone(best), id)
		}
	}
	return s.p2Pick(best)
}

func (s *State) p2Pick(group []int) int {
	chosen := group[0]
	for _, id := range group[1:] {
		if s.lowerPreferred(id, chosen) {
			chosen = id
		}
	}
	return chosen
}

func (s *State) p2Blame() (Action, bool) {
	x, ok := s.p2Find()
	if !ok {
		return Action{}, false
	}
	seq := x.f.seq
	blamed := s.p2Blamed(x)
	if x.rd != nil && s.round == x.rd {
		return Action{Kind: Decide, Payload: &journal.DeepeningRound{Round: x.rd.start.Round, Event: journal.CycleEnd, Reason: fmt.Sprintf("%s: failure #%d, blame goes to the deepened cores", p2Tag(), seq)}, Cause: []int{seq}}, true
	}
	if x.rd != nil {
		for _, c := range s.cores {
			id, i := c.id, s.index(c.id)
			if id == blamed || !slices.Contains(x.rd.start.Cores, id) || x.rd.start.Profile[i] >= x.rd.initial[i] || c.offset == x.rd.initial[i] {
				continue
			}
			return Action{Kind: Decide, Payload: &journal.TunerDecision{Core: id, Phase: journal.PhaseDeepening, Decision: journal.Yield, FromOffset: c.offset, ToOffset: x.rd.initial[i], Pass: c.pass, FailurePoint: c.fail, Reason: fmt.Sprintf("%s: round %d failed on core %02d, so core %02d returns to %d and may retry its target", p2Tag(), x.rd.start.Round, blamed, id, x.rd.initial[i])}, Cause: []int{seq}}, true
		}
	}
	c := s.core(blamed)
	i := s.index(blamed)
	failedAt := x.profile[i]
	if x.f.failure.Core != nil && *x.f.failure.Core == blamed && x.f.failure.Offset != nil {
		failedAt = *x.f.failure.Offset
	}
	fail := failedAt
	if c.fail != nil {
		fail = max(fail, *c.fail)
	}
	back := s.p2.conf[blamed]
	phase := journal.PhaseChecking
	if x.rd != nil {
		back = x.rd.initial[i]
		phase = journal.PhaseDeepening
	}
	to := max(c.offset, back, fail+1)
	pass, _ := keepPass(c.pass, fail)
	where := fmt.Sprintf("phase 2 confirmation, last confirmed offset %d", s.p2.conf[blamed])
	if x.rd != nil {
		where = fmt.Sprintf("round %d, last passed offset %d", x.rd.start.Round, back)
	}
	reason := fmt.Sprintf("%s: failure #%d (%s trial %s) charged to deepened core %02d; %s; failure point %d, the core is done", p2Tag(), seq, x.f.failure.Regime, x.f.failure.Trial, blamed, where, fail)
	return Action{Kind: Decide, Payload: &journal.TunerDecision{Core: blamed, Phase: phase, Decision: journal.Backoff, FromOffset: c.offset, ToOffset: to, Pass: pass, FailurePoint: new(fail), Reason: reason}, Cause: []int{seq}}, true
}
