package tuner

import (
	"fmt"
	"slices"
	"strings"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/requests"
)

// moveMark is the round that moved a core to its current unverified offset and where that round began.
type moveMark struct{ round, seq int }

// phases is the state of the two-phase method, rebuilt from the journal. Phase 1 ends at the first passed full cycle
// after every core left search; phase 2 is the rounds after it and the confirmation cycle that concludes it.
type phases struct {
	phase1End    int
	confirmed    []int
	passed       []int
	moves        map[int]moveMark
	candidates   []pendingFailure
	lastRound    *round
	confirmStart int
	concluded    int
}

func (s *State) foldPhaseCycleStart(e journal.Event) {
	if ph := &s.phases; ph.phase1End != 0 && ph.confirmStart == 0 {
		ph.confirmStart = e.Seq
	}
}

func (s *State) foldPhaseCycleEnd(e journal.Event) {
	ph := &s.phases
	switch {
	case ph.phase1End == 0:
		if !s.anySearch() {
			ph.phase1End = e.Seq
			ph.confirmed = slices.Clone(s.checking.profile)
			ph.passed = slices.Clone(s.checking.profile)
		}
	case ph.confirmStart != 0 && ph.concluded == 0:
		ph.concluded = e.Seq
		ph.candidates = nil
	}
}

func (s *State) foldPhaseRoundStart(e journal.Event, p *journal.DeepeningRound, initial []int) {
	ph := &s.phases
	for i, c := range s.byID() {
		if slices.Contains(p.Cores, c.id) && i < len(p.Profile) && p.Profile[i] < initial[i] && c.offset == initial[i] {
			if ph.moves == nil {
				ph.moves = map[int]moveMark{}
			}
			ph.moves[c.id] = moveMark{p.Round, e.Seq}
		}
	}
}

func (s *State) foldPhaseRoundEnd(p *journal.DeepeningRound) {
	ph := &s.phases
	ph.lastRound = s.round
	if !p.Passed || len(ph.passed) != len(s.cores) {
		return
	}
	for i, c := range s.byID() {
		if slices.Contains(s.round.start.Cores, c.id) {
			ph.passed[i] = s.round.start.Profile[i]
			delete(ph.moves, c.id)
		}
	}
}

func (s *State) trackPhaseFailure(f pendingFailure) {
	if ph := &s.phases; ph.phase1End != 0 && ph.concluded == 0 && !f.carried && f.failure.KnownFailure == 0 {
		ph.candidates = append(ph.candidates, f)
	}
}

// consumePhase2 settles every failure a phase-2 backoff cites: no other decision answers it.
func (s *State) consumePhase2(e journal.Event, p *journal.TunerDecision) {
	ph := &s.phases
	if p.Phase != journal.PhaseDeepening || p.Decision != journal.Backoff || ph.phase1End == 0 || e.Seq <= ph.phase1End {
		return
	}
	for _, seq := range e.Cause {
		s.queue = slices.DeleteFunc(s.queue, func(f pendingFailure) bool { return f.seq == seq })
		ph.candidates = slices.DeleteFunc(ph.candidates, func(f pendingFailure) bool { return f.seq == seq })
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

func coreList(ids []int) string {
	parts := make([]string, len(ids))
	for i, id := range ids {
		parts[i] = fmt.Sprintf("%02d", id)
	}
	return strings.Join(parts, ", ")
}

// phase2Moves returns the profile one round moves to, the cores it moves one count toward their solo limits, and the
// cores that kept an unverified offset from a failed round and are checked again without moving.
func (s *State) phase2Moves() (profile, moved, carried []int) {
	profile = s.offsets()
	passed := s.phases.passed
	for _, c := range s.cores {
		i := s.index(c.id)
		switch {
		case c.phase == journal.PhaseSearch:
		case i < len(passed) && c.offset < passed[i]:
			carried = append(carried, c.id)
		case !c.hasSoloLimit || c.offset <= c.soloLimit:
		default:
			q := slices.Clone(profile)
			q[i] = c.offset - 1
			if _, hit := s.reaches(q); !hit {
				profile, moved = q, append(moved, c.id)
			}
		}
	}
	return
}

func (s *State) phase2RoundDue() bool {
	ph := &s.phases
	if ph.phase1End == 0 || ph.confirmStart != 0 || s.checking.open || s.round != nil || s.hunt != nil || len(s.queue) > 0 || len(s.obligations) > 0 || s.anySearch() {
		return false
	}
	_, moved, carried := s.phase2Moves()
	return len(moved)+len(carried) > 0
}

func (s *State) phase2RoundStart() Action {
	profile, moved, carried := s.phase2Moves()
	cores := append(slices.Clone(moved), carried...)
	slices.Sort(cores)
	target := s.offsets()
	for _, id := range cores {
		target[s.index(id)] = s.core(id).soloLimit
	}
	base := s.passedFullCycles[len(s.passedFullCycles)-1]
	reason := fmt.Sprintf("; phase 2: cores %s move one count toward their solo limits", coreList(moved))
	if len(carried) > 0 {
		reason += fmt.Sprintf("; cores %s kept their offset from the failed round and are checked again", coreList(carried))
	}
	return Action{Kind: Decide, Payload: &journal.DeepeningRound{Round: s.nextRound + 1, Event: journal.CycleStart, Base: slices.Clone(base.profile), BaseSeq: base.seq, Target: target, Profile: profile, Cores: cores, Ranking: slices.Clone(s.ranking), Trials: s.n, TrialS: s.durations.ShortTrialS, Reason: reason}, Cause: []int{base.seq}}
}

func (s *State) cycleStartReason() string {
	if ph := &s.phases; ph.phase1End != 0 && ph.concluded == 0 {
		return "; phase 2 confirmation cycle"
	}
	return ""
}

func (s *State) cycleEndReason(full bool) string {
	ph := &s.phases
	switch {
	case !full:
	case ph.phase1End == 0 && !s.anySearch():
		_, moved, _ := s.phase2Moves()
		if len(moved) == 0 {
			return "; phase 1 ends: first confirmed profile; no core can move, phase 2's confirmation cycle follows"
		}
		return fmt.Sprintf("; phase 1 ends: first confirmed profile; movable cores %s", coreList(moved))
	case ph.confirmStart != 0 && ph.concluded == 0:
		return "; phase 2 ends: the confirmation cycle passed, so this is the confirmed profile"
	}
	return ""
}

type blame struct {
	failure  pendingFailure
	intent   *journal.TrialIntent
	round    *round
	loaded   []int
	deepened []int
	profile  []int
}

func (s *State) phase2Find() (blame, bool) {
	ph := &s.phases
	for _, f := range ph.candidates {
		b := blame{failure: f, intent: s.intents[f.failure.Trial]}
		if b.intent != nil && b.intent.Round > 0 {
			for _, r := range []*round{s.round, ph.lastRound} {
				if r != nil && r.start.Round == b.intent.Round {
					b.round = r
					break
				}
			}
		}
		if b.round == nil && (ph.confirmStart == 0 || f.seq < ph.confirmStart) {
			continue
		}
		b.profile = f.profile
		if len(b.profile) != len(s.cores) && b.intent != nil {
			b.profile = b.intent.Profile
		}
		if len(b.profile) != len(s.cores) && b.round != nil {
			b.profile = b.round.start.Profile
		}
		switch {
		case b.intent == nil:
			b.loaded = s.ids()
		case b.intent.Core != nil:
			b.loaded = []int{*b.intent.Core}
		case len(b.intent.Cores) > 0:
			b.loaded = b.intent.Cores
		default:
			b.loaded = s.ids()
		}
		for _, id := range b.loaded {
			i := s.index(id)
			if b.round != nil {
				if slices.Contains(b.round.start.Cores, id) && b.round.start.Profile[i] < b.round.initial[i] {
					b.deepened = append(b.deepened, id)
				}
			} else if len(b.profile) == len(s.cores) && b.profile[i] < ph.confirmed[i] {
				b.deepened = append(b.deepened, id)
			}
		}
		if len(b.deepened) > 0 {
			return b, true
		}
	}
	return blame{}, false
}

func (s *State) phase2Blamed(b blame) int {
	d := b.deepened
	if f := b.failure.failure; f.Attribution == journal.Attributed && f.Core != nil && slices.Contains(d, *f.Core) {
		return *f.Core
	}
	if s.multiR7(b.failure.class) {
		if failed := s.r7FailureEntry(b.failure); failed != nil {
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
				return s.preferredShallow(groups[0])
			}
		}
	}
	move := func(id int) int {
		if b.round != nil {
			return b.round.start.Profile[s.index(id)] - b.round.initial[s.index(id)]
		}
		return b.profile[s.index(id)] - s.phases.confirmed[s.index(id)]
	}
	deepest := d[:1]
	for _, id := range d[1:] {
		switch {
		case move(id) < move(deepest[0]):
			deepest = []int{id}
		case move(id) == move(deepest[0]):
			deepest = append(slices.Clone(deepest), id)
		}
	}
	return s.preferredShallow(deepest)
}

func (s *State) preferredShallow(group []int) int {
	chosen := group[0]
	for _, id := range group[1:] {
		if s.lowerPreferred(id, chosen) {
			chosen = id
		}
	}
	return chosen
}

// phase2Blame answers a failure with a deepened core loaded: it ends the round, returns the other deepened movers to
// their last passed offsets, then backs the blamed core off to its last passed offset past its new failure point.
func (s *State) phase2Blame() (Action, bool) {
	b, ok := s.phase2Find()
	if !ok {
		return Action{}, false
	}
	seq := b.failure.seq
	blamed := s.phase2Blamed(b)
	if b.round != nil && s.round == b.round {
		return Action{Kind: Decide, Payload: &journal.DeepeningRound{Round: b.round.start.Round, Event: journal.CycleEnd, Reason: fmt.Sprintf("failure #%d: blame goes to the deepened cores", seq)}, Cause: []int{seq}}, true
	}
	if b.round != nil {
		for _, c := range s.cores {
			i := s.index(c.id)
			mover := slices.Contains(b.round.start.Cores, c.id) && b.round.start.Profile[i] < b.round.initial[i]
			if c.id == blamed || !mover || c.offset >= b.round.initial[i] || !slices.Contains(b.loaded, c.id) {
				continue
			}
			return Action{Kind: Decide, Payload: &journal.TunerDecision{Core: c.id, Phase: journal.PhaseDeepening, Decision: journal.Yield, FromOffset: c.offset, ToOffset: b.round.initial[i], Pass: c.pass, FailurePoint: c.fail, Reason: fmt.Sprintf("round %d failed on core %02d, so core %02d returns to its last passed offset %d", b.round.start.Round, blamed, c.id, b.round.initial[i])}, Cause: []int{seq}}, true
		}
	}
	c := s.core(blamed)
	i := s.index(blamed)
	failedAt := b.profile[i]
	if f := b.failure.failure; f.Core != nil && *f.Core == blamed && f.Offset != nil {
		failedAt = *f.Offset
	}
	fail := failedAt
	if c.fail != nil {
		fail = max(fail, *c.fail)
	}
	back := s.phases.confirmed[i]
	where := fmt.Sprintf("phase 2 confirmation, last confirmed offset %d", back)
	if b.round != nil {
		back = b.round.initial[i]
		where = fmt.Sprintf("round %d, last passed offset %d", b.round.start.Round, back)
	}
	to := max(c.offset, back, fail+1)
	pass, _ := keepPass(c.pass, fail)
	f := b.failure.failure
	reason := fmt.Sprintf("failure #%d (%s trial %s) charged to deepened core %02d; %s; failure point %d", seq, f.Regime, f.Trial, blamed, where, fail)
	return Action{Kind: Decide, Payload: &journal.TunerDecision{Core: blamed, Phase: journal.PhaseDeepening, Decision: journal.Backoff, FromOffset: c.offset, ToOffset: to, Pass: pass, FailurePoint: new(fail), Reason: reason}, Cause: []int{seq}}, true
}
