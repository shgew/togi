package tuner

import (
	"fmt"
	"slices"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type round struct {
	start   *journal.DeepeningRound
	seq     int
	initial []int
}

func (s *State) foldRound(e journal.Event, p *journal.DeepeningRound) {
	if p.Event == journal.LapStart {
		s.round = &round{start: p, seq: e.Seq, initial: s.offsets()}
		s.nextRound = max(s.nextRound, p.Round)
		s.lastPlanSeq = e.Seq
	} else if s.round != nil && p.Round == s.round.start.Round {
		s.round = nil
	}
	s.projectionDirty = true
}

func totalDepth(p []int) int {
	sum := 0
	for _, v := range p {
		sum += v
	}
	return sum
}

func (s *State) deepeningDue() bool { return !s.checking.open && s.canDeepen() }

func (s *State) CanDeepen() bool { return s.canDeepen() }

func (s *State) canDeepen() bool {
	if s.hunt != nil || len(s.queue) > 0 || len(s.obligations) > 0 || s.anySearch() || len(s.passedFullLaps) == 0 {
		return false
	}
	p := s.offsets()
	target := s.best()
	if target == nil {
		return false
	}
	if totalDepth(target) < totalDepth(p) {
		return true
	}
	return slices.ContainsFunc(s.cores, func(c *core) bool { return c.phase == journal.PhaseHasRoom })
}

func (s *State) roundStart() Action {
	p := s.offsets()
	target := s.best()
	q := slices.Clone(p)
	var changed []int
	for i, c := range s.byID() {
		if target[i] > p[i] {
			q[i] = target[i]
		} else if target[i] < p[i] {
			q[i] = p[i] - (p[i]-target[i]+1)/2
		}
		if q[i] != p[i] {
			changed = append(changed, c.id)
		}
	}
	parked := s.passedFullLaps[len(s.passedFullLaps)-1]
	return Action{Kind: Decide, Payload: &journal.DeepeningRound{Round: s.nextRound + 1, Event: journal.LapStart, Parked: slices.Clone(parked.profile), ParkedSeq: parked.seq, Target: target, Profile: q, Cores: changed, Ranking: slices.Clone(s.ranking), Starts: s.n, StartS: s.durations.StartS}, Cause: []int{parked.seq}}
}

func (s *State) roundMoves() (Action, bool) {
	r := s.round
	if r == nil {
		return Action{}, false
	}
	if reachedConstraint, ok := s.reaches(r.start.Profile); ok {
		return Action{Kind: Decide, Payload: &journal.DeepeningRound{Round: r.start.Round, Event: journal.LapEnd, Reason: "its profile reaches " + reachedConstraint}, Cause: []int{r.seq}}, true
	}
	for _, yield := range []bool{true, false} {
		for _, c := range s.cores {
			i := s.index(c.id)
			target := r.start.Profile[i]
			if !slices.Contains(r.start.Cores, c.id) || c.offset == target || yield != (target > r.initial[i]) {
				continue
			}
			decision := journal.Deepen
			reason := fmt.Sprintf("round %d: halfway toward %d", r.start.Round, r.start.Target[i])
			if yield {
				decision = journal.Yield
				reason = fmt.Sprintf("round %d: yields to %d so the profile can reach %d counts", r.start.Round, target, -totalDepth(r.start.Target))
			}
			return Action{Kind: Decide, Payload: &journal.TunerDecision{Core: c.id, Phase: journal.PhaseDeepening, Decision: decision, FromOffset: c.offset, ToOffset: target, Pass: c.pass, FailurePoint: c.fail, Reason: reason}, Cause: []int{r.seq}}, true
		}
	}
	return Action{}, false
}

func (s *State) roundChecks() []requirement {
	r := s.round
	if r == nil {
		return nil
	}
	p := r.start.Profile
	var out []requirement
	index := (r.start.Round - 1) % 3
	for _, c := range s.cores {
		i := s.index(c.id)
		if !slices.Contains(r.start.Cores, c.id) || p[i] >= r.initial[i] {
			continue
		}
		for _, regime := range []machine.Regime{machine.R1, machine.R2} {
			w := machine.Workloads(regime)[index].ID
			out = append(out, requirement{class: trialClass{regime, w, coresKey([]int{c.id}), r.start.StartS}, cores: []int{c.id}, core: c.id, offset: p[i], count: r.start.Starts})
		}
	}
	for _, part := range s.parts {
		needed := false
		for _, id := range part {
			idx := s.index(id)
			if slices.Contains(r.start.Cores, id) && p[idx] < r.initial[idx] {
				needed = true
				break
			}
		}
		if needed {
			w := machine.Workloads(machine.R7)[index].ID
			out = append(out, requirement{class: trialClass{machine.R7, w, coresKey(part), r.start.StartS}, cores: slices.Clone(part), count: r.start.Starts})
		}
	}
	return out
}

func (s *State) roundCheck() Action {
	r := s.round
	cause := []int{r.seq}
	for _, q := range s.roundChecks() {
		seqs := s.passSeqs(q.class, r.start.Profile, r.seq, deepeningEvidence)
		if len(seqs) >= q.count {
			cause = s.citeCarried(cause, seqs[:q.count]...)
			continue
		}
		if s.retry != nil && s.retry.Round == r.start.Round {
			return Action{Kind: RunTrial, Trial: *s.retry, Cause: []int{r.seq}}
		}
		t := Trial{Regime: q.class.regime, Workload: q.class.workload, Condition: machine.Together, Phase: journal.PhaseDeepening, DurationS: q.class.duration, Round: r.start.Round}
		if q.class.regime == machine.R7 {
			t.Cores = q.cores
		} else {
			t.Core, t.Offset = q.core, q.offset
		}
		return Action{Kind: RunTrial, Trial: t, Cause: []int{r.seq}}
	}
	return Action{Kind: Decide, Payload: &journal.DeepeningRound{Round: r.start.Round, Event: journal.LapEnd, Passed: true, Reason: s.carriedReason(cause)}, Cause: cause}
}

func (s *State) projectRound() *journal.DeepeningState {
	r := s.round
	if r == nil {
		return nil
	}
	st := &journal.DeepeningState{Round: r.start.Round, Seq: r.seq, Target: slices.Clone(r.start.Target), Profile: slices.Clone(r.start.Profile), Cores: slices.Clone(r.start.Cores)}
	for _, q := range s.roundChecks() {
		st.Checks = append(st.Checks, journal.CheckState{Regime: q.class.regime, Workload: q.class.workload, Cores: slices.Clone(q.cores), Passes: s.passes(q.class, r.start.Profile, r.seq, deepeningEvidence), Needed: q.count})
	}
	return st
}
