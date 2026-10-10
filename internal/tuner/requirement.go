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

func newRequirement(kind string, k trialClass, since int, rule evidenceRule, needed int) ScheduledRequirement {
	return ScheduledRequirement{Kind: kind, Class: ScheduledClass{Regime: k.regime, Workload: k.workload, Cores: k.cores, DurationS: k.duration}, Since: since, Rule: rule, Needed: needed}
}

// scheduledFor reconstructs the requirement of a recorded intent from the journal alone. Fold stores it per trial; a
// builder never calls it for a trial it chose the requirement of.
func (s *State) scheduledFor(p *journal.TrialIntent) scheduledTrial {
	k := classOf(p)
	q := newRequirement("unclassified", k, 0, cycleEvidence, 1)
	switch {
	case p.Condition == machine.Alone && p.Round == 0:
		var c *core
		if p.Core != nil {
			c = s.core(*p.Core)
		}
		q = s.soloRequirement(c, k)
	case p.Hunt > 0 && s.hunt != nil:
		q = newRequirement("hunt", k, 0, huntEvidence, s.hunt.start.Trials)
		for _, g := range s.hunt.groups {
			if g.payload.Group == p.Group {
				q = s.huntRequirement(k, g)
				break
			}
		}
	case p.Rerun && len(s.obligations) > 0:
		q = s.rerunRequirement(k)
	case p.Round > 0 && s.round != nil:
		needed := 1
		for _, r := range s.roundChecks() {
			if r.class == k {
				needed = r.count
				break
			}
		}
		q = s.deepeningRequirement(k, needed)
	case p.Cycle > 0:
		q.Since = s.checking.startSeq
		n := 0
		if p.Step == 0 {
			n = s.passes(k, p.Profile, q.Since, q.Rule)
		}
		first, last := 0, len(s.checking.steps)
		if p.Step > 0 && p.Step <= last {
			first, last = p.Step-1, p.Step
		}
		for i := first; i < last; i++ {
			req := s.requirements(i)
			for j, r := range req {
				if r.class == k && r.count > 0 && (p.Step > 0 || n < r.count) {
					cycle, part := s.cycleRequirement(req, i, j)
					return scheduledTrial{requirement: cycle, part: part}
				}
			}
		}
	}
	return scheduledTrial{requirement: q}
}

func (s *State) soloRequirement(c *core, k trialClass) ScheduledRequirement {
	if c != nil && c.check {
		return newRequirement("solo-limit", k, c.phaseSeq, soloLimitEvidence, s.n)
	}
	return newRequirement("search", k, 0, cycleEvidence, 1)
}

func (s *State) huntRequirement(k trialClass, g groupRecord) ScheduledRequirement {
	return newRequirement("hunt", k, s.inferenceSince(g.payload, g.seq), huntEvidence, s.hunt.start.Trials)
}

// rerunRequirement is the requirement of a rerun of class k: the oldest rerun obligation's, or none, which leaves the
// rerun unclassified.
func (s *State) rerunRequirement(k trialClass) ScheduledRequirement {
	if len(s.obligations) == 0 {
		return newRequirement("unclassified", k, 0, cycleEvidence, 1)
	}
	needed := 1
	if k.duration == s.durations.ShortTrialS {
		needed = s.n
	}
	return newRequirement("rerun", k, s.obligations[0].seq, rerunEvidence, needed)
}

func (s *State) deepeningRequirement(k trialClass, needed int) ScheduledRequirement {
	return newRequirement("deepening", k, s.round.seq, deepeningEvidence, needed)
}

// cycleRequirement is the obligation of req[j], one of step's requirements, and the part holding it: the run of
// consecutive requirements on the same cores. Step and Part are one-based ordinals; Needed is the class's count.
func (s *State) cycleRequirement(req []requirement, step, j int) (ScheduledRequirement, []requirement) {
	part := 0
	for a := 0; ; {
		b := a + 1
		for b < len(req) && slices.Equal(req[a].cores, req[b].cores) {
			b++
		}
		part++
		if j < b {
			q := newRequirement("cycle", req[j].class, s.checking.startSeq, cycleEvidence, req[j].count)
			q.Step, q.Part = step+1, part
			return q, req[a:b]
		}
		a = b
	}
}

// shapeWorkload is the workload the trial will run: an unset one is the next in the core's rotation.
func (s *State) shapeWorkload(t Trial) string {
	if t.Workload != "" {
		return t.Workload
	}
	index := 0
	if c := s.core(t.Core); c != nil {
		index = c.workloadIndex[t.Regime]
	}
	return machine.PickWorkload(t.Regime, index).ID
}

// shapeClass is the class the journal will record for trial t, for builders that choose a requirement by the shape
// they built rather than by a class they selected.
func (s *State) shapeClass(t Trial) trialClass {
	cores := t.Cores
	if len(cores) == 0 {
		cores = []int{t.Core}
	} else if !slices.IsSorted(cores) {
		cores = slices.Sorted(slices.Values(cores))
	}
	return trialClass{t.Regime, s.shapeWorkload(t), coresKey(cores), t.DurationS}
}

// runTrial schedules t with the requirement its builder selected.
func (s *State) runTrial(t Trial, q ScheduledRequirement, cause []int) Action {
	t.Requirement = q
	return Action{Kind: RunTrial, Trial: t, Cause: cause}
}

// runRetry schedules retry t. When t repeats the selected requirement q, q is its requirement; a retry keeps the shape
// of its earlier intent, so after a config or profile change it may not, and then it takes the requirement the fold
// will reconstruct for it.
func (s *State) runRetry(t Trial, q ScheduledRequirement, cause []int) Action {
	if s.retryRepeats(t, q) {
		return s.runTrial(t, q, cause)
	}
	return s.retryTrial(t, cause)
}

func (s *State) retryRepeats(t Trial, q ScheduledRequirement) bool {
	if t.Hunt != 0 || t.Rerun || s.shapeClass(t) != q.class() {
		return false
	}
	switch q.Kind {
	case "cycle":
		current := len(t.Profile) == 0 || slices.Equal(t.Profile, s.checking.profile)
		return t.Cycle > 0 && t.Round == 0 && t.Condition != machine.Alone && (t.Step == q.Step || t.Step == 0 && current)
	case "deepening":
		return t.Round > 0
	}
	return false
}

// retryTrial schedules a retry whose requirement is reconstructed from its shape as the fold will reconstruct it.
func (s *State) retryTrial(t Trial, cause []int) Action {
	p := journal.TrialIntent{Regime: t.Regime, Workload: s.shapeWorkload(t), DurationS: t.DurationS, Condition: t.Condition, Phase: t.Phase, Cores: t.Cores, Profile: t.Profile, Cycle: t.Cycle, Hunt: t.Hunt, Group: t.Group, Round: t.Round, Rerun: t.Rerun, Step: t.Step}
	if len(t.Cores) == 0 {
		p.Core, p.Offset = &t.Core, &t.Offset
	}
	if len(p.Profile) == 0 {
		p.Profile = s.checking.profile
	}
	return s.runTrial(t, s.scheduledFor(&p).requirement, cause)
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
		selected, passed, needed := q.class(), 0, 0
		for _, r := range stored.part {
			if r.count == 0 {
				continue
			}
			if r.class == selected {
				passed += h.Requirement.Passed
			} else {
				passed += s.passes(r.class, in.Profile, q.Since, q.Rule)
			}
			needed += r.count
		}
		h.PartTrial, h.PartNeeded = min(passed+1, max(needed, 1)), needed
	}
	stored.ended = &h
	s.scheduled[p.Trial] = stored
}
