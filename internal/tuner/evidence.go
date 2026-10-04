package tuner

import (
	"fmt"
	"slices"
	"strconv"
	"strings"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type trialClass struct {
	regime   machine.Regime
	workload string
	cores    string
	duration int
}

type classTarget struct {
	cores []int
	multi bool
}

func (s *State) indexClassTargets() {
	s.classTargets = make(map[string]classTarget, len(s.cores)+len(s.parts)+1)
	for _, c := range s.byID() {
		cores := []int{c.id}
		s.classTargets[coresKey(cores)] = classTarget{cores: cores}
	}
	for _, part := range s.parts {
		key := coresKey(part)
		if target := s.classTargets[key]; !target.multi {
			s.classTargets[key] = classTarget{cores: part, multi: true}
		}
	}
	s.classTargets["[]"] = classTarget{cores: []int{}}
}

type entry struct {
	seq       int
	profile   []int
	pass      bool
	condition machine.Condition
	class     trialClass
	tctlMax   int
	hasTctl   bool
	carried   bool
	cores     []int
}

func classOf(p *journal.TrialIntent) trialClass {
	cores := slices.Clone(p.Cores)
	if p.Core != nil {
		cores = []int{*p.Core}
	}
	slices.Sort(cores)
	return trialClass{p.Regime, p.Workload, coresKey(cores), p.DurationS}
}

// coresKey prints cores as fmt.Sprint does, without reflection: class keys are built on every projection.
func coresKey(cores []int) string {
	b := make([]byte, 0, 2+3*len(cores))
	b = append(b, '[')
	for i, c := range cores {
		if i > 0 {
			b = append(b, ' ')
		}
		b = strconv.AppendInt(b, int64(c), 10)
	}
	return string(append(b, ']'))
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

type evidenceRule uint8

const (
	allEvidence evidenceRule = iota
	soloLimitEvidence
	huntEvidence
	rerunEvidence
	deepeningEvidence
	cycleEvidence
)

func (r evidenceRule) admits(e *entry, since int) bool {
	if e.carried {
		return r != cycleEvidence
	}
	return e.seq > since
}

func (s *State) latestFailure(k trialClass, p []int, since int) int {
	last := since
	entries := s.ledger[k]
	for i := range entries {
		e := &entries[i]
		if !e.pass && e.seq > last && atLeastShallow(e.profile, p) {
			last = e.seq
		}
	}
	if k.regime == machine.R6 && len(s.idle) > 0 && k.cores == coresKey(s.ids()) {
		for i := range s.idle {
			e := &s.idle[i]
			if e.seq > last && atLeastShallow(e.profile, p) {
				last = e.seq
			}
		}
	}
	return last
}

func (s *State) passSeqs(k trialClass, p []int, since int, rule evidenceRule) []int {
	last := s.latestFailure(k, p, 0)
	var seqs []int
	entries := s.ledger[k]
	for i := range entries {
		e := &entries[i]
		if e.pass && rule.admits(e, since) && e.seq > last && atLeastDeep(e.profile, p) {
			seqs = append(seqs, e.seq)
		}
	}
	return seqs
}

func (s *State) passes(k trialClass, p []int, since int, rule evidenceRule) int {
	last := s.latestFailure(k, p, 0)
	count := 0
	entries := s.ledger[k]
	for i := range entries {
		e := &entries[i]
		if e.pass && rule.admits(e, since) && e.seq > last && atLeastDeep(e.profile, p) {
			count++
		}
	}
	return count
}

func (s *State) failingSeq(k trialClass, p []int, since int) int {
	if s.passes(k, p, 0, allEvidence) >= s.n {
		return 0
	}
	last := admittedFailure(s.ledger[k], p, since, 0)
	if k.regime == machine.R6 && len(s.idle) > 0 && k.cores == coresKey(s.ids()) {
		last = admittedFailure(s.idle, p, since, last)
	}
	return last
}

func admittedFailure(entries []entry, p []int, since, last int) int {
	for i := range entries {
		e := &entries[i]
		if !e.pass && e.seq > last && allEvidence.admits(e, since) && atLeastShallow(e.profile, p) {
			last = e.seq
		}
	}
	return last
}

func (s *State) fails(k trialClass, p []int, since int) bool {
	return s.failingSeq(k, p, since) != 0
}

func (s *State) citeCarried(cause []int, seqs ...int) []int {
	for _, seq := range seqs {
		if _, carried := s.carriedSources[seq]; carried && !slices.Contains(cause, seq) {
			cause = append(cause, seq)
		}
	}
	return cause
}

func (s *State) carriedReason(seqs []int) string {
	var sessions []string
	count := 0
	for _, seq := range seqs {
		if source, ok := s.carriedSources[seq]; ok {
			count++
			if !slices.Contains(sessions, source) {
				sessions = append(sessions, source)
			}
		}
	}
	if count == 0 {
		return ""
	}
	slices.Sort(sessions)
	return fmt.Sprintf("; %d carried facts from session %s", count, strings.Join(sessions, ", "))
}

func (s *State) recordEvidence(ev journal.Event, p *journal.TrialIntent, end *journal.TrialEnd) {
	if p.RecordOnly || end.Outcome != journal.OutcomePass && end.Outcome != journal.OutcomeFailure {
		return
	}
	k := classOf(p)
	profile := slices.Clone(p.Profile)
	if len(profile) != len(s.cores) {
		profile = make([]int, len(s.cores))
		if p.Core != nil && p.Offset != nil {
			profile[s.index(*p.Core)] = *p.Offset
		} else {
			copy(profile, s.checking.profile)
		}
	}
	if end.Outcome == journal.OutcomeFailure {
		seqs := s.passSeqs(k, profile, 0, allEvidence)
		if len(seqs) >= s.n && s.n > 0 {
			s.warning = &journal.TunerWarning{Warning: "monotonicity", Trial: p.Trial, Passes: slices.Clone(seqs[:s.n]), Detail: fmt.Sprintf("failure at profile %v contradicts %d valid passes in %s %s", profile, len(seqs), k.regime, k.workload)}
			s.warningSeq = ev.Seq
		}
	}
	e := entry{seq: ev.Seq, profile: profile, pass: end.Outcome == journal.OutcomePass, condition: p.Condition, class: k, cores: p.Cores}
	if p.Core != nil {
		e.cores = []int{*p.Core}
	}
	if carried, ok := ev.Data.(*journal.TrialCarried); ok {
		e.carried = true
		s.carriedSources[ev.Seq] = carried.Source.Session
	}
	if end.TctlMaxC != nil {
		e.tctlMax, e.hasTctl = *end.TctlMaxC, true
	}
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
	var contradicted []int
	var class trialClass
	if s.n > 0 {
		all := coresKey(s.ids())
		for k := range s.ledger {
			if k.regime != machine.R6 || k.cores != all {
				continue
			}
			seqs := s.passSeqs(k, p.Profile, 0, allEvidence)
			if len(seqs) >= s.n && (contradicted == nil || seqs[0] < contradicted[0]) {
				contradicted, class = seqs[:s.n], k
			}
		}
	}
	if contradicted != nil {
		s.warning = &journal.TunerWarning{Warning: "monotonicity", Passes: contradicted, Detail: fmt.Sprintf("idle failure at profile %v contradicts %d valid passes in %s %s at %ds", p.Profile, s.n, class.regime, class.workload, class.duration)}
		s.warningSeq = ev.Seq
	}
	cores := s.ids()
	e := entry{seq: ev.Seq, profile: slices.Clone(p.Profile), class: trialClass{regime: machine.R6, cores: coresKey(cores)}, cores: cores}
	if carried, ok := ev.Data.(*journal.FailureCarried); ok {
		e.carried = true
		s.carriedSources[ev.Seq] = carried.Source.Session
	}
	s.idle = append(s.idle, e)
	s.failures = append(s.failures, e)
	s.projectionDirty = true
}

func (s *State) recordCarried(ev journal.Event, p *journal.TrialCarried) {
	if p.RecordOnly {
		return
	}
	intent := &journal.TrialIntent{Regime: p.Class.Regime, Workload: p.Class.Workload, Cores: p.Class.Cores, DurationS: p.Class.DurationS, Condition: p.Condition, Profile: p.Profile}
	end := &journal.TrialEnd{Trial: p.Source.Trial, Outcome: p.Outcome, Signal: p.Signal, DurationS: p.DurationS}
	s.recordEvidence(ev, intent, end)
	if p.Outcome == journal.OutcomeFailure {
		evidence := s.failures[len(s.failures)-1]
		failure := &journal.Failure{Signal: p.Signal, Attribution: journal.Unattributed, Core: p.Core, Trial: p.Source.Trial, Regime: p.Class.Regime, Condition: p.Condition, Profile: evidence.profile}
		if failure.Core == nil && p.Condition == machine.Alone && len(p.Class.Cores) == 1 {
			failure.Core = new(p.Class.Cores[0])
		}
		if failure.Core != nil {
			if i := s.index(*failure.Core); i >= 0 && i < len(p.Profile) {
				failure.Attribution, failure.Offset = journal.Attributed, new(p.Profile[i])
			}
		}
		s.rememberCarriedFailure(ev.Seq, failure, evidence.class)
	}
}

func (s *State) rememberCarriedFailure(seq int, p *journal.Failure, k trialClass) {
	s.failureIndex[seq] = len(s.pendingFailures)
	s.pendingFailures = append(s.pendingFailures, pendingFailure{seq: seq, failure: p, profile: p.Profile, class: k, carried: true})
}

func (s *State) failureAfter(f pendingFailure, since int) bool {
	if f.carried {
		return s.failureBySeq(f.seq) != nil
	}
	return f.seq > since
}

func (s *State) resetEvidence(core int) {
	involves := func(e entry) bool {
		if slices.Contains(e.cores, core) {
			delete(s.carriedSources, e.seq)
			return true
		}
		return false
	}
	for k, entries := range s.ledger {
		s.ledger[k] = slices.DeleteFunc(entries, involves)
	}
	s.idle = slices.DeleteFunc(s.idle, involves)
	s.failures = slices.DeleteFunc(s.failures, involves)
	s.projectionDirty = true
	s.rerunCauses = nil
}

func (s *State) queueRerun(ev journal.Event) {
	for _, cause := range ev.Cause {
		if f := s.failureBySeq(cause); f != nil {
			if f.class.workload != "" {
				s.obligations = append(s.obligations, rerun{f.class, cause})
				return
			}
			if f.failure.Trial == "" {
				k := trialClass{machine.R6, machine.Workloads(machine.R6)[0].ID, coresKey(s.ids()), s.durations.CheckingIdleS}
				s.obligations = append(s.obligations, rerun{k, cause})
				return
			}
		}
	}
	if s.hunt != nil {
		if s.hunt.directFailure > 0 {
			if f := s.failureBySeq(s.hunt.directFailure); f != nil {
				if f.class.workload != "" {
					s.obligations = append(s.obligations, rerun{f.class, f.seq})
					return
				}
			}
		}
		s.obligations = append(s.obligations, rerun{s.hunt.class, s.hunt.start.Failure})
	}
}
