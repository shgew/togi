package tuner

import (
	"slices"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

// ScheduledRequirement is the evidence obligation attached to a scheduled trial.
// Step and Part are one-based identities, separate from the class's trial count.
type ScheduledRequirement struct {
	Kind               string
	Class              ScheduledClass
	Since              int
	Rule               evidenceRule
	Needed, Step, Part int
}

type ScheduledClass struct {
	Regime          machine.Regime
	Workload, Cores string
	DurationS       int
}

type scheduledTrial struct {
	requirement ScheduledRequirement
	part        []requirement
	ended       *TrialHistory
}

// TrialHistory retains progress immediately before the trial ended. Part counts
// preserve the dashboard's existing combined short/long presentation.
type TrialHistory struct {
	Requirement           TrialRequirement
	Step                  int
	PartTrial, PartNeeded int
}

func (q ScheduledRequirement) class() trialClass {
	return trialClass{q.Class.Regime, q.Class.Workload, q.Class.Cores, q.Class.DurationS}
}

// StoredRequirement returns the obligation captured before folding trial.intent.
func (s *State) StoredRequirement(trial string) ScheduledRequirement {
	return s.scheduled[trial].requirement
}

func (s *State) TrialHistory(trial string) TrialHistory {
	if h := s.scheduled[trial].ended; h != nil {
		return *h
	}
	return TrialHistory{}
}

func (s *State) scheduledFor(p *journal.TrialIntent) scheduledTrial {
	k := classOf(p)
	q := ScheduledRequirement{Kind: "unclassified", Class: ScheduledClass{Regime: k.regime, Workload: k.workload, Cores: k.cores, DurationS: k.duration}, Rule: cycleEvidence, Needed: 1}
	out := scheduledTrial{}
	switch {
	case p.Condition == machine.Alone && p.Round == 0:
		var c *core
		if p.Core != nil {
			c = s.core(*p.Core)
		}
		q.Kind = "search"
		if c != nil && c.check {
			q.Kind, q.Since, q.Rule, q.Needed = "solo-limit", c.phaseSeq, soloLimitEvidence, s.n
		}
	case p.Hunt > 0 && s.hunt != nil:
		q.Kind, q.Rule, q.Needed = "hunt", huntEvidence, s.hunt.start.Trials
		for _, g := range s.hunt.groups {
			if g.payload.Group == p.Group {
				q.Since = s.inferenceSince(g.payload, g.seq)
				break
			}
		}
	case p.Rerun && len(s.obligations) > 0:
		q.Kind, q.Since, q.Rule = "rerun", s.obligations[0].seq, rerunEvidence
		if p.DurationS == s.durations.ShortTrialS {
			q.Needed = s.n
		}
	case p.Round > 0 && s.round != nil:
		q.Kind, q.Since, q.Rule = "deepening", s.round.seq, deepeningEvidence
		for _, r := range s.roundChecks() {
			if r.class == k {
				q.Needed = r.count
				break
			}
		}
	case p.Cycle > 0:
		q.Since = s.checking.startSeq
		n := s.passes(k, p.Profile, q.Since, q.Rule)
		first, last := 0, len(s.checking.steps)
		if p.Step > 0 && p.Step <= last {
			first, last = p.Step-1, p.Step
		}
		for i := first; i < last; i++ {
			req := s.requirements(i)
			part := 0
			for a := 0; a < len(req); {
				b := a + 1
				for b < len(req) && slices.Equal(req[a].cores, req[b].cores) {
					b++
				}
				part++
				for _, r := range req[a:b] {
					if r.class == k && r.count > 0 && (p.Step > 0 || n < r.count) {
						q.Kind, q.Needed, q.Step, q.Part = "cycle", r.count, i+1, part
						out.requirement, out.part = q, req[a:b]
						return out
					}
				}
				a = b
			}
		}
	}
	out.requirement = q
	return out
}

func (s *State) runTrial(t Trial, cause []int) Action {
	workload := t.Workload
	if workload == "" {
		index := 0
		if c := s.core(t.Core); c != nil {
			index = c.workloadIndex[t.Regime]
		}
		workload = machine.PickWorkload(t.Regime, index).ID
	}
	p := journal.TrialIntent{Regime: t.Regime, Workload: workload, DurationS: t.DurationS, Condition: t.Condition, Phase: t.Phase, Cores: t.Cores, Profile: t.Profile, Cycle: t.Cycle, Hunt: t.Hunt, Group: t.Group, Round: t.Round, Rerun: t.Rerun, Step: t.Step}
	if len(t.Cores) == 0 {
		p.Core, p.Offset = &t.Core, &t.Offset
	}
	if len(p.Profile) == 0 {
		p.Profile = s.checking.profile
	}
	q := s.scheduledFor(&p).requirement
	return Action{Kind: RunTrial, Trial: q.trial(t), Cause: cause}
}

func (q ScheduledRequirement) trial(shape Trial) Trial {
	shape.Regime, shape.DurationS = q.Class.Regime, q.Class.DurationS
	if shape.Workload != "" {
		shape.Workload = q.Class.Workload
	}
	shape.Requirement = q
	return shape
}

func (s *State) requirementProgress(p *journal.TrialIntent, q ScheduledRequirement) TrialRequirement {
	if q.Kind == "search" {
		return TrialRequirement{Trial: 1, Needed: 1}
	}
	k := q.class()
	n := s.passes(k, p.Profile, q.Since, q.Rule)
	r := TrialRequirement{Passed: n, Trial: n + 1, Needed: q.Needed}
	if q.Kind == "cycle" {
		r.Failed = s.cycleFailures(k, p.Profile, q.Since)
	}
	return r
}

func (s *State) recordTrialHistory(p *journal.TrialEnd) {
	in := s.intents[p.Trial]
	if in == nil {
		return
	}
	stored := s.scheduled[p.Trial]
	q := stored.requirement
	h := TrialHistory{Requirement: s.requirementProgress(in, q), Step: q.Step}
	if in.Cycle > 0 && in.Cycle == s.checking.cycle && len(stored.part) > 0 {
		passed, needed := 0, 0
		for _, r := range stored.part {
			if r.count > 0 {
				passed += s.passes(r.class, in.Profile, q.Since, q.Rule)
				needed += r.count
			}
		}
		h.PartTrial, h.PartNeeded = min(passed+1, max(needed, 1)), needed
	}
	stored.ended = &h
	s.scheduled[p.Trial] = stored
}
