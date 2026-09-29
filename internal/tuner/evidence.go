package tuner

import (
	"fmt"
	"slices"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type trialClass struct {
	regime   machine.Regime
	workload string
	cores    string
	duration int
}

type entry struct {
	seq       int
	profile   []int
	pass      bool
	duration  int
	condition machine.Condition
	class     trialClass
}

func classOf(p *journal.TrialIntent) trialClass {
	cores := slices.Clone(p.Cores)
	if p.Core != nil {
		cores = []int{*p.Core}
	}
	slices.Sort(cores)
	return trialClass{p.Regime, p.Workload, fmt.Sprint(cores), p.DurationS}
}

func (k trialClass) withDuration(d int) trialClass { k.duration = d; return k }
func atLeastDeep(q, p []int) bool {
	if len(q) != len(p) {
		return false
	}
	for i, v := range p {
		if q[i] > v {
			return false
		}
	}
	return true
}

func atLeastShallow(q, p []int) bool { return atLeastDeep(p, q) }
func allZero(p []int) bool {
	for _, v := range p {
		if v != 0 {
			return false
		}
	}
	return true
}

func (s *State) latestFailure(k trialClass, p []int, since int) int {
	last := since
	for _, e := range s.ledger[k] {
		if !e.pass && e.seq > last && atLeastShallow(e.profile, p) {
			last = e.seq
		}
	}
	if k.regime == machine.R6 && k.cores == fmt.Sprint(s.ids()) {
		for _, e := range s.idle {
			if e.seq > last && atLeastShallow(e.profile, p) {
				last = e.seq
			}
		}
	}
	return last
}

func (s *State) passSeqs(k trialClass, p []int, since int) []int {
	last := s.latestFailure(k, p, since)
	var seqs []int
	for _, e := range s.ledger[k] {
		if e.pass && e.seq > last && atLeastDeep(e.profile, p) {
			seqs = append(seqs, e.seq)
		}
	}
	return seqs
}

func (s *State) passes(k trialClass, p []int, since int) int { return len(s.passSeqs(k, p, since)) }
func (s *State) fails(k trialClass, p []int, since int) bool {
	return s.latestFailure(k, p, since) > since
}

func (s *State) recordEvidence(ev journal.Event, p *journal.TrialIntent, end *journal.TrialEnd) {
	if end.Outcome != journal.OutcomePass && end.Outcome != journal.OutcomeFailure {
		return
	}
	k := classOf(p)
	profile := slices.Clone(p.Profile)
	if len(profile) != len(s.cores) {
		profile = make([]int, len(s.cores))
		if p.Core != nil && p.Offset != nil {
			profile[s.index(*p.Core)] = *p.Offset
		} else {
			copy(profile, s.guard.profile)
		}
	}
	if end.Outcome == journal.OutcomeFailure {
		seqs := s.passSeqs(k, profile, 0)
		if len(seqs) >= s.n && s.n > 0 {
			s.warning = &journal.TunerWarning{Warning: "monotonicity", Trial: p.Trial, Passes: slices.Clone(seqs[:s.n]), Detail: fmt.Sprintf("failure at profile %v contradicts %d valid passes in %s %s", profile, len(seqs), k.regime, k.workload)}
			s.warningSeq = ev.Seq
		}
	}
	e := entry{seq: ev.Seq, profile: profile, pass: end.Outcome == journal.OutcomePass, duration: end.DurationS, condition: p.Condition, class: k}
	s.ledger[k] = append(s.ledger[k], e)
	if !e.pass {
		s.failures = append(s.failures, e)
	}
	s.projectionDirty = true
}

func (s *State) recordIdle(ev journal.Event, p *journal.Failure) {
	if len(p.Profile) != len(s.cores) {
		return
	}
	e := entry{seq: ev.Seq, profile: slices.Clone(p.Profile), class: trialClass{regime: machine.R6, cores: fmt.Sprint(s.ids())}}
	s.idle = append(s.idle, e)
	s.failures = append(s.failures, e)
	s.projectionDirty = true
}

func (s *State) queueRerun(ev journal.Event) {
	for _, cause := range ev.Cause {
		if f := s.failureBySeq(cause); f != nil {
			if intent := s.intents[f.failure.Trial]; intent != nil {
				s.obligations = append(s.obligations, rerun{classOf(intent), cause})
				return
			}
			if f.failure.Trial == "" {
				k := trialClass{machine.R6, machine.Workloads(machine.R6)[0].ID, fmt.Sprint(s.ids()), s.durations.GuardIdleS}
				s.obligations = append(s.obligations, rerun{k, cause})
				return
			}
		}
	}
	if s.hunt != nil {
		if s.hunt.directFailure > 0 {
			if f := s.failureBySeq(s.hunt.directFailure); f != nil {
				if intent := s.intents[f.failure.Trial]; intent != nil {
					s.obligations = append(s.obligations, rerun{classOf(intent), f.seq})
					return
				}
			}
		}
		s.obligations = append(s.obligations, rerun{s.hunt.class, s.hunt.start.Failure})
	}
}
