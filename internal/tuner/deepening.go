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
		initial := slices.Clone(s.phases.passed)
		if initial == nil {
			initial = s.offsets()
		}
		s.foldPhaseRoundStart(e, p, initial)
		s.round = &round{start: p, seq: e.Seq, initial: initial}
		s.nextRound = max(s.nextRound, p.Round)
		s.lastPlanSeq = e.Seq
	} else if s.round != nil && p.Round == s.round.start.Round {
		s.foldPhaseRoundEnd(p)
		s.round = nil
	}
	s.projectionDirty = true
}

func (s *State) roundMoves() (Action, bool) {
	r := s.round
	if r == nil {
		return Action{}, false
	}
	if reachedConstraint, ok := s.reaches(r.start.Profile); ok {
		return Action{Kind: Decide, Payload: &journal.DeepeningRound{Round: r.start.Round, Event: journal.CycleEnd, Reason: "its profile reaches " + reachedConstraint}, Cause: []int{r.seq}}, true
	}
	for _, c := range s.cores {
		target := r.start.Profile[s.index(c.id)]
		if !slices.Contains(r.start.Cores, c.id) || c.offset == target {
			continue
		}
		return Action{Kind: Decide, Payload: &journal.TunerDecision{Core: c.id, Phase: journal.PhaseDeepening, Decision: journal.Deepen, FromOffset: c.offset, ToOffset: target, Pass: c.pass, FailurePoint: c.fail, Reason: fmt.Sprintf("round %d: one count toward solo limit %d", r.start.Round, r.start.Target[s.index(c.id)])}, Cause: []int{r.seq}}, true
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
	for _, c := range s.cores {
		i := s.index(c.id)
		if !slices.Contains(r.start.Cores, c.id) || p[i] >= r.initial[i] {
			continue
		}
		mark, ok := s.phases.moves[c.id]
		if !ok {
			mark = moveMark{r.start.Round, r.seq}
		}
		for _, regime := range []machine.Regime{machine.R1, machine.R2} {
			w := machine.Workloads(regime)[(mark.round-1)%3].ID
			out = append(out, requirement{class: trialClass{regime, w, coresKey([]int{c.id}), r.start.TrialS}, cores: []int{c.id}, core: c.id, offset: p[i], count: r.start.Trials, since: mark.seq})
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
				out = append(out, requirement{class: k, cores: slices.Clone(previous), count: r.start.Trials, since: r.seq})
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
		seqs := s.passSeqs(q.class, s.roundCheckProfile(q), q.since, deepeningEvidence)
		if len(seqs) >= q.count {
			cause = s.citeCarried(cause, seqs[:q.count]...)
			continue
		}
		selected := s.deepeningRequirement(q.class, q.count, q.since)
		if t, ok := s.retryFor(selected); ok {
			return s.runTrial(t, selected, []int{r.seq})
		}
		t := Trial{Regime: q.class.regime, Workload: q.class.workload, Condition: machine.Together, Phase: journal.PhaseDeepening, DurationS: q.class.duration, Round: r.start.Round}
		if q.class.regime == machine.R7 {
			t.Cores = q.cores
			t.Profile = r.start.Profile
		} else {
			t.Core, t.Offset = q.core, q.offset
			t.Condition = machine.Alone
		}
		return s.runTrial(t, selected, cause)
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
		st.Checks = append(st.Checks, journal.CheckState{Regime: q.class.regime, Workload: q.class.workload, Cores: slices.Clone(q.cores), Passes: s.passes(q.class, s.roundCheckProfile(q), q.since, deepeningEvidence), Needed: q.count})
	}
	return st
}
