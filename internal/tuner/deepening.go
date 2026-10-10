package tuner

import (
	"fmt"
	"slices"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/requests"
)

type round struct {
	start   *journal.DeepeningRound
	seq     int
	initial []int
}

func (s *State) foldRound(e journal.Event, p *journal.DeepeningRound) {
	if p.Event == journal.CycleStart {
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

func (s *State) canDeepen() bool {
	if s.hunt != nil || len(s.queue) > 0 || len(s.obligations) > 0 || s.anySearch() || len(s.passedFullCycles) == 0 {
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
	base := s.passedFullCycles[len(s.passedFullCycles)-1]
	return Action{Kind: Decide, Payload: &journal.DeepeningRound{Round: s.nextRound + 1, Event: journal.CycleStart, Base: slices.Clone(base.profile), BaseSeq: base.seq, Target: target, Profile: q, Cores: changed, Ranking: slices.Clone(s.ranking), Trials: s.n, TrialS: s.durations.ShortTrialS}, Cause: []int{base.seq}}
}

func (s *State) roundMoves() (Action, bool) {
	r := s.round
	if r == nil {
		return Action{}, false
	}
	if reachedConstraint, ok := s.reaches(r.start.Profile); ok {
		return Action{Kind: Decide, Payload: &journal.DeepeningRound{Round: r.start.Round, Event: journal.CycleEnd, Reason: "its profile reaches " + reachedConstraint}, Cause: []int{r.seq}}, true
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

// roundChecksWithSources returns the round's checks and the measurements that ordered them. Callers must not modify
// the result: it is memoized until the next fold.
func (s *State) roundChecksWithSources() ([]requirement, []int) {
	if s.roundGen == s.gen && s.roundMemo {
		return s.roundChecksMemo, s.roundSourcesMemo
	}
	out, sources := s.computeRoundChecks()
	s.roundChecksMemo, s.roundSourcesMemo, s.roundGen, s.roundMemo = out, sources, s.gen, true
	return out, sources
}

func (s *State) computeRoundChecks() ([]requirement, []int) {
	r := s.round
	if r == nil {
		return nil, nil
	}
	p := r.start.Profile
	var out []requirement
	var sources []int
	index := (r.start.Round - 1) % 3
	for _, c := range s.cores {
		i := s.index(c.id)
		if !slices.Contains(r.start.Cores, c.id) || p[i] >= r.initial[i] {
			continue
		}
		for _, regime := range []machine.Regime{machine.R1, machine.R2} {
			w := machine.Workloads(regime)[index].ID
			out = append(out, requirement{class: trialClass{regime, w, coresKey([]int{c.id}), r.start.TrialS}, cores: []int{c.id}, core: c.id, offset: p[i], count: r.start.Trials})
		}
	}

	for _, workload := range machine.Workloads(machine.R7) {
		for _, full := range s.ccdParts() {
			out, sources = s.roundR7Checks(workload.ID, full, out, sources)
		}
	}
	return out, sources
}

func (s *State) roundDeepened(cores []int) bool {
	r := s.round
	return slices.ContainsFunc(cores, func(id int) bool {
		return slices.Contains(r.start.Cores, id) && r.start.Profile[s.index(id)] < r.initial[s.index(id)]
	})
}

func (s *State) roundR7Checks(workload string, full []int, out []requirement, sources []int) ([]requirement, []int) {
	r := s.round
	cite := func(seqs []int) {
		for _, seq := range seqs {
			if !slices.Contains(sources, seq) {
				sources = append(sources, seq)
			}
		}
	}
	initialRequests, initialSources := s.r7Requests(workload, full, r.initial)
	cite(initialSources)
	var initialTop []int
	if groups := requests.Groups(initialRequests); len(groups) > 0 {
		initialTop = groups[0]
	}
	for previous := full; len(previous) > 0; {
		voltages, seqs := s.r7Requests(workload, previous, r.start.Profile)
		cite(seqs)
		groups := requests.Groups(voltages)
		if len(groups) == 0 {
			break
		}
		if s.roundDeepened(groups[0]) || slices.Equal(previous, full) && s.roundDeepened(initialTop) {
			k := trialClass{machine.R7, workload, coresKey(previous), r.start.TrialS}
			if !slices.ContainsFunc(out, func(q requirement) bool { return q.class == k }) {
				out = append(out, requirement{class: k, cores: slices.Clone(previous), count: r.start.Trials})
			}
		}
		next := make([]int, 0, len(previous)-len(groups[0]))
		for _, id := range previous {
			if !slices.Contains(groups[0], id) {
				next = append(next, id)
			}
		}
		previous = next
		if len(previous) < 2 {
			break
		}
	}
	return out, sources
}

func (s *State) roundChecks() []requirement {
	checks, _ := s.roundChecksWithSources()
	return checks
}

func (s *State) roundCheckProfile(q requirement) []int {
	if q.class.regime == machine.R7 {
		return s.round.start.Profile
	}
	profile := make([]int, len(s.cores))
	profile[s.index(q.core)] = q.offset
	return profile
}

func (s *State) roundCheck() Action {
	r := s.round
	checks, sources := s.roundChecksWithSources()
	cause := append([]int{r.seq}, sources...)
	for _, q := range checks {
		seqs := s.passSeqs(q.class, s.roundCheckProfile(q), r.seq, deepeningEvidence)
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
			t.Profile = r.start.Profile
		} else {
			t.Core, t.Offset = q.core, q.offset
			t.Condition = machine.Alone
		}
		return Action{Kind: RunTrial, Trial: t, Cause: cause}
	}
	return Action{Kind: Decide, Payload: &journal.DeepeningRound{Round: r.start.Round, Event: journal.CycleEnd, Passed: true, Reason: s.carriedReason(cause)}, Cause: cause}
}

func (s *State) projectRound() *journal.DeepeningState {
	r := s.round
	if r == nil {
		return nil
	}
	st := &journal.DeepeningState{Round: r.start.Round, Seq: r.seq, Target: slices.Clone(r.start.Target), Profile: slices.Clone(r.start.Profile), Cores: slices.Clone(r.start.Cores)}
	for _, q := range s.roundChecks() {
		st.Checks = append(st.Checks, journal.CheckState{Regime: q.class.regime, Workload: q.class.workload, Cores: slices.Clone(q.cores), Passes: s.passes(q.class, s.roundCheckProfile(q), r.seq, deepeningEvidence), Needed: q.count})
	}
	return st
}
