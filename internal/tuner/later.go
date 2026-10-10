package tuner

import (
	"fmt"
	"slices"

	"github.com/shgew/togi/internal/journal"
)

// laterWindow is K: two failures against one core pair when they fall within this many cycle starts (ADR 0056).
const laterWindow = 5

// strike is a held first failure with the cores it counted against and the offsets they had when it happened. back
// holds the offset each core the hold steps back would show in the BIOS profile while the hold is valid: the ordinary
// step-back's target for an attributed or multi-core R7 hold, one count shallower for an unattributed hold's candidates.
type strike struct {
	failure, seq, cycle int
	cores, offsets      []int
	back                map[int]int
}

// laterState is rebuilt from the journal. Holds are tuner.decision backoffs that move nothing, and hunt.skipped events,
// that the fold tells apart from answers to stale failures and from skips of profiles that reach a constraint.
type laterState struct {
	strikes      []strike
	held         map[int]bool
	confirmed    []int
	confirmedSeq int
	moved        map[int]int
}

// BIOSProfile is the profile to enter in BIOS: the last confirmed profile with every later step-back applied, and every
// valid hold's step-back shown at once.
type BIOSProfile struct {
	Offsets []int
	// Confirmed is the sequence of the passed full cycle that confirmed the profile; 0 before phase 1 ends.
	Confirmed int
	// Unconfirmed lists, ascending, the cores showing an offset that is not the confirmed one: stepped back, or shown
	// stepped back by a valid hold.
	Unconfirmed []int
	// Since is the sequence of the earliest event that left the profile unconfirmed; 0 when it is confirmed.
	Since int
}

// BIOSProfile derives the profile to enter in BIOS from the folded events. A step-back replaces it at the decision
// that records it. A held failure shows the step-back it would have made, until a second failure's step-back replaces
// it, a paired hunt's commitment (or its end without one) does, the core moves for another reason (the shown offset
// then follows the core) or K cycles pass (it returns to the held offset). The next passed full cycle confirms what
// the profile then is.
func (s *State) BIOSProfile() BIOSProfile {
	out := BIOSProfile{Offsets: s.offsets(), Confirmed: s.later.confirmedSeq}
	confirmed := s.later.confirmed
	if len(confirmed) != len(out.Offsets) {
		return out
	}
	since := map[int]int{}
	for i, c := range s.byID() {
		out.Offsets[i] = max(confirmed[i], c.offset)
		if out.Offsets[i] != confirmed[i] {
			seq := s.later.moved[c.id]
			if seq == 0 {
				seq = c.decisionSeq
			}
			since[c.id] = seq
		}
	}
	shown := s.later.strikes
	if h := s.hunt; h != nil && h.pairedStrike != nil {
		shown = append(slices.Clone(shown), *h.pairedStrike)
	}
	for _, k := range shown {
		if !s.strikeValid(k) {
			continue
		}
		for id, to := range k.back {
			i := s.index(id)
			if i < 0 || to <= confirmed[i] {
				continue
			}
			out.Offsets[i] = max(out.Offsets[i], to)
			if seq, ok := since[id]; !ok || k.seq < seq {
				since[id] = k.seq
			}
		}
	}
	for id, seq := range since {
		out.Unconfirmed = append(out.Unconfirmed, id)
		if out.Since == 0 || seq < out.Since {
			out.Since = seq
		}
	}
	slices.Sort(out.Unconfirmed)
	return out
}

func (s *State) foldLaterCycleEnd(e journal.Event) {
	if s.phases.phase1End == 0 {
		return
	}
	s.later.confirmed = slices.Clone(s.checking.profile)
	s.later.confirmedSeq = e.Seq
	s.later.moved = nil
	s.pruneStrikes()
}

func (s *State) strikeValid(k strike) bool {
	for i, id := range k.cores {
		if c := s.core(id); c == nil || c.offset != k.offsets[i] {
			return false
		}
	}
	return true
}

// pruneStrikes drops every strike no later failure can pair with: a struck core moved, or the window passed.
func (s *State) pruneStrikes() {
	s.later.strikes = slices.DeleteFunc(s.later.strikes, func(k strike) bool {
		return !s.strikeValid(k) || s.checking.cycle-k.cycle >= laterWindow
	})
}

// laterGate reports whether a first later failure is held: phase 2 concluded and the current profile has passed a full
// cycle since it last changed.
func (s *State) laterGate() bool {
	if s.phases.concluded == 0 || len(s.passedFullCycles) == 0 {
		return false
	}
	last := s.passedFullCycles[len(s.passedFullCycles)-1]
	return last.seq > s.phases.concluded && last.seq > s.checking.profileSeq && slices.Equal(s.checking.profile, s.offsets())
}

// laterFailure returns the failure a later-failure rule can hold or pair: a live failure observed after phase 2
// concluded, not a known-failure skip.
func (s *State) laterFailure(seq int) *pendingFailure {
	if s.phases.concluded == 0 {
		return nil
	}
	if _, located := s.located[seq]; located {
		return nil
	}
	f := s.failureBySeq(seq)
	if f == nil || f.carried || f.failure.KnownFailure != 0 || f.seq <= s.phases.concluded || f.failure.Offset != nil && *f.failure.Offset == 0 {
		return nil
	}
	return f
}

// pairedStrike returns the oldest valid strike of an earlier failure within the window that counts against one of cores.
func (s *State) pairedStrike(f *pendingFailure, cores []int) *strike {
	for i := range s.later.strikes {
		k := &s.later.strikes[i]
		if k.failure != f.seq && f.cycle >= k.cycle && f.cycle-k.cycle < laterWindow && s.strikeValid(*k) && slices.ContainsFunc(k.cores, func(id int) bool { return slices.Contains(cores, id) }) {
			return k
		}
	}
	return nil
}

func (s *State) pairClause(k *strike, f *pendingFailure) string {
	return fmt.Sprintf("second failure within %d cycles: #%d (cycle %d, held) and #%d (cycle %d)", laterWindow, k.failure, k.cycle, f.seq, f.cycle)
}

func deadline(cycle int) int { return cycle + laterWindow - 1 }

// laterDecision answers an ordinary step-back after phase 2 concluded: a second failure against a core within the
// window steps it back and names both failures; a first one, once the profile has passed a cycle, holds.
func (s *State) laterDecision(a Action) Action {
	p, ok := a.Payload.(*journal.TunerDecision)
	if !ok || a.Kind != Decide || p.Phase != journal.PhaseChecking || p.Decision != journal.Backoff || p.ToOffset == p.FromOffset || len(a.Cause) == 0 {
		return a
	}
	f := s.laterFailure(a.Cause[0])
	c := s.core(p.Core)
	if f == nil || c == nil {
		return a
	}
	if k := s.pairedStrike(f, []int{c.id}); k != nil {
		paired := *p
		paired.Reason += "; " + s.pairClause(k, f)
		a.Payload = &paired
		a.Cause = append(slices.Clone(a.Cause), k.failure)
		return a
	}
	if !s.laterGate() {
		return a
	}
	last := s.passedFullCycles[len(s.passedFullCycles)-1]
	if f.seq <= last.seq || !s.heldAgainst(f, c) {
		return a
	}
	reason := fmt.Sprintf("holds at %d: failure #%d (cycle %d) is the first counting against core %02d since the profile passed cycle %d (#%d); a second failure against it by cycle %d (K = %d) steps it back", c.offset, f.seq, f.cycle, c.id, last.cycle, last.seq, deadline(f.cycle), laterWindow)
	a.Payload = &journal.TunerDecision{Core: c.id, Phase: journal.PhaseChecking, Decision: journal.Backoff, FromOffset: c.offset, ToOffset: c.offset, Pass: c.pass, FailurePoint: c.fail, Reason: reason}
	return a
}

// heldAgainst reports whether f failed core c at its current offset, so that holding leaves c exactly where f found it.
func (s *State) heldAgainst(f *pendingFailure, c *core) bool {
	i := s.index(c.id)
	return i >= 0 && i < len(f.profile) && f.profile[i] == c.offset
}

// laterHunt answers the failure at the head of the hunt queue after phase 2 concluded: it holds a first failure
// loading cores no earlier held failure counts against, and returns the strike a second one pairs with.
func (s *State) laterHunt(f pendingFailure, cores []int) (*Action, *strike) {
	live := s.laterFailure(f.seq)
	if live == nil {
		return nil, nil
	}
	if k := s.pairedStrike(live, cores); k != nil {
		return nil, k
	}
	if !s.laterGate() {
		return nil, nil
	}
	last := s.passedFullCycles[len(s.passedFullCycles)-1]
	if f.seq <= last.seq {
		return nil, nil
	}
	reason := fmt.Sprintf("held: first unattributed failure loading cores [%s] since the profile passed cycle %d (#%d); a second failure loading any of them by cycle %d (K = %d) is hunted", coreList(cores), last.cycle, last.seq, deadline(f.cycle), laterWindow)
	return &Action{Kind: Decide, Payload: &journal.HuntSkipped{Failure: f.seq, Reason: reason}, Cause: []int{f.seq}}, nil
}

// releaseStrike consumes the strike an event cites last among at least two causes, when it counts against one of
// cores, and returns the event without that cause so the ordinary bookkeeping sees only the failure it answers, and the
// strike it consumed.
func (s *State) releaseStrike(e journal.Event, cores []int) (journal.Event, *strike) {
	n := len(e.Cause)
	if n < 2 || len(s.later.strikes) == 0 {
		return e, nil
	}
	failure := e.Cause[n-1]
	i := slices.IndexFunc(s.later.strikes, func(k strike) bool {
		return k.failure == failure && slices.ContainsFunc(k.cores, func(id int) bool { return slices.Contains(cores, id) })
	})
	if i < 0 {
		return e, nil
	}
	k := s.later.strikes[i]
	s.later.strikes = slices.Delete(s.later.strikes, i, i+1)
	e.Cause = e.Cause[:n-1]
	return e, &k
}

// laterHold returns the failure a decision holds: a checking backoff that moves nothing and answers a live failure
// that applied the core's current offset, which an answer to a stale failure at a deeper offset never does.
func (s *State) laterHold(e journal.Event, p *journal.TunerDecision) *pendingFailure {
	if p.Phase != journal.PhaseChecking || p.Decision != journal.Backoff || p.ToOffset != p.FromOffset || len(e.Cause) == 0 {
		return nil
	}
	f := s.laterFailure(e.Cause[0])
	if f == nil {
		return nil
	}
	i := s.index(p.Core)
	if i < 0 || i >= len(f.profile) || f.profile[i] != p.FromOffset {
		return nil
	}
	return f
}

// holdBack returns the offset a hold of core p.Core shows in the BIOS profile: the target of the ordinary decision the
// hold replaced. The fold has not yet applied the hold, so the state is the one Next decided from, and
// ordinaryDecision changes no decision state, only caches. It returns nil when that decision is not a step-back of the
// same core answering the same failure.
func (s *State) holdBack(e journal.Event, p *journal.TunerDecision) map[int]int {
	a, ok := s.ordinaryDecision()
	if !ok || len(a.Cause) == 0 || a.Cause[0] != e.Cause[0] {
		return nil
	}
	d, ok := a.Payload.(*journal.TunerDecision)
	if !ok || d.Core != p.Core || d.Decision != journal.Backoff || d.FromOffset != p.FromOffset || d.ToOffset <= d.FromOffset {
		return nil
	}
	return map[int]int{p.Core: d.ToOffset}
}

// huntBack returns the offsets a held hunt.skipped of f shows: every candidate the skipped hunt would have searched,
// one count shallower. Cores at 0 stay.
func (s *State) huntBack(f pendingFailure) map[int]int {
	_, _, candidates := s.huntCandidates(f)
	back := map[int]int{}
	for _, id := range candidates {
		if c := s.core(id); c != nil && c.offset < 0 {
			back[id] = c.offset + 1
		}
	}
	return back
}

func (s *State) addStrike(f *pendingFailure, seq int, cores []int, back map[int]int) {
	k := strike{failure: f.seq, seq: seq, cycle: f.cycle, cores: slices.Clone(cores), back: back}
	for _, id := range cores {
		k.offsets = append(k.offsets, s.core(id).offset)
	}
	s.later.strikes = append(s.later.strikes, k)
	if s.later.held == nil {
		s.later.held = map[int]bool{}
	}
	s.later.held[f.seq] = true
}

// laterHuntHold returns the failure a hunt.skipped holds: a live failure after conclusion whose profile reaches no
// recorded constraint, which the ordinary skip never answers.
func (s *State) laterHuntHold(p *journal.HuntSkipped) *pendingFailure {
	f := s.laterFailure(p.Failure)
	if f == nil {
		return nil
	}
	if _, reaches := s.reaches(f.profile); reaches {
		return nil
	}
	return f
}

func (s *State) noteMove(seq int, c *core) {
	i := s.index(c.id)
	if i < 0 || i >= len(s.later.confirmed) || c.offset <= s.later.confirmed[i] || s.later.moved[c.id] != 0 {
		return
	}
	if s.later.moved == nil {
		s.later.moved = map[int]int{}
	}
	s.later.moved[c.id] = seq
}

// pairedClause says which two failures a paired hunt answers.
func (h *hunt) pairedClause() string {
	if h.paired == 0 {
		return ""
	}
	return fmt.Sprintf("; after failures #%d and #%d", min(h.paired, h.start.Failure), max(h.paired, h.start.Failure))
}
