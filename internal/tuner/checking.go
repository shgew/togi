package tuner

import (
	"fmt"
	"slices"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type checking struct {
	profile    []int
	profileSeq int
	cycle      int
	open       bool
	startSeq   int
	steps      []machine.Regime
	stepsDone  int
	lastSeq    int
	partial    map[int]*checkingStep
}

type checkingStep struct {
	start  *journal.CheckingStep
	seq    int
	chains map[int][]*checkingChain
}

type checkingChain struct {
	start *journal.CheckingChain
	seq   int
}

type requirement struct {
	class  trialClass
	cores  []int
	count  int
	core   int
	offset int
	since  int
}

func (s *State) fullCycleCoverage(steps []machine.Regime) (bool, []string) {
	want := map[machine.Regime]int{machine.R1: 3, machine.R2: 3, machine.R3: 1, machine.R4: 1, machine.R5: 1, machine.R6: 1, machine.R7: 3}
	var missing []string
	for _, r := range machine.Regimes {
		got := 0
		for _, step := range steps {
			if step == r {
				got++
			}
		}
		if need := want[r] - got; need > 0 {
			missing = append(missing, fmt.Sprintf("%s: %d more steps", r, need))
		}
	}
	return len(missing) == 0, missing
}

func (s *State) foldCycle(e journal.Event, p *journal.CheckingCycle) {
	g := &s.checking
	g.lastSeq = e.Seq
	if p.Event == journal.CycleStart {
		g.cycle, g.open, g.startSeq = p.Cycle, true, e.Seq
		g.steps = slices.Clone(p.Steps)
		g.stepsDone = 0
		g.partial = map[int]*checkingStep{}
		s.foldPhaseCycleStart(e)
		s.pruneStrikes()
		s.projectionDirty = true
		return
	}
	g.stepsDone = len(g.steps)
	if !p.Passed {
		g.stepsDone = s.checkingStepsDone()
	}
	g.open = false
	if p.Passed && p.Full {
		s.passedFullCycles = append(s.passedFullCycles, passedFullCycle{profile: slices.Clone(g.profile), seq: e.Seq, cycle: p.Cycle})
		s.foldPhaseCycleEnd(e)
		s.foldLaterCycleEnd(e)
	}
	s.projectionDirty = true
}

func (s *State) longS(part []int) int {
	d := s.durations.CheckingAllCoreS
	groups := len(s.parts) - 1
	if groups <= 1 {
		return d
	}
	if len(part) < len(s.cores) {
		return d / 4
	}
	return d - groups*(d/4)
}

// requirements returns step's checking requirements. Callers must not modify the result: it is memoized until an
// event changes the checking cycle, the checking profile, core offsets or durations.
func (s *State) requirements(step int) []requirement {
	if s.reqEpoch != s.checkingEpoch || s.reqByStep == nil {
		if s.reqByStep == nil {
			s.reqByStep = map[int][]requirement{}
		}
		clear(s.reqByStep)
		s.reqEpoch = s.checkingEpoch
	}
	if req, ok := s.reqByStep[step]; ok {
		return req
	}
	req := s.computeRequirements(step)
	s.reqByStep[step] = req
	return req
}

func (s *State) computeRequirements(step int) []requirement {
	g := &s.checking
	r := g.steps[step]
	w := s.stepWorkload(step)
	var req []requirement
	add := func(cores []int, d, count int, core, offset int) {
		k := trialClass{r, w, coresKey(cores), d}
		total := count
		for i := 0; i <= step; i++ {
			if i == step {
				break
			}
			if g.steps[i] != r {
				continue
			}
			if s.stepWorkload(i) != w {
				continue
			}
			if r == machine.R7 {
				for _, part := range s.r7StepParts(i) {
					if slices.Equal(part, cores) {
						total += count
					}
				}
			} else {
				total += count
			}
		}
		req = append(req, requirement{class: k, cores: slices.Clone(cores), count: total, core: core, offset: offset})
	}
	switch r {
	case machine.R1, machine.R2, machine.R3, machine.R4, machine.R5:
		for _, c := range s.cores {
			add([]int{c.id}, s.durations.CheckingTrialS, 1, c.id, c.offset)
		}
	case machine.R6:
		add(s.ids(), s.durations.CheckingIdleS, 1, 0, 0)
	case machine.R7:
		for _, part := range s.r7StepParts(step) {
			add(part, s.durations.ShortTrialS, 3, 0, 0)
			add(part, s.longS(part), 1, 0, 0)
		}
	}
	for i := range req {
		for j := range i {
			if req[j].class == req[i].class {
				req[i].count += req[j].count
				req[j].count = 0
			}
		}
	}
	return req
}

func (s *State) cycleNext() Action {
	g := &s.checking
	for i := 0; i < len(g.steps); i++ {
		if g.steps[i] == machine.R7 {
			if a, pending := s.r7StepNext(i); pending {
				return a
			}
			continue
		}
		req := s.requirements(i)
		for j, q := range req {
			if q.count == 0 {
				continue
			}
			if s.cyclePasses(q.class) >= q.count {
				continue
			}
			selected, _ := s.cycleRequirement(req, i, j)
			if t, ok := s.retryFor(selected); ok {
				return s.runTrial(t, selected, []int{g.lastSeq})
			}
			t := Trial{Regime: q.class.regime, Workload: q.class.workload, DurationS: q.class.duration, Phase: journal.PhaseChecking, Condition: machine.Together, Cycle: g.cycle}
			if q.class.regime == machine.R6 || q.class.regime == machine.R7 {
				t.Cores = q.cores
			} else {
				t.Core, t.Offset = q.core, q.offset
			}
			return s.runTrial(t, selected, []int{g.lastSeq})
		}
	}
	fullCycleCoverage, missing := s.fullCycleCoverage(g.steps)
	return Action{Kind: Decide, Payload: &journal.CheckingCycle{Cycle: g.cycle, Event: journal.CycleEnd, Passed: true, Full: fullCycleCoverage, Missing: missing, Reason: s.cycleEndReason(fullCycleCoverage)}, Cause: []int{g.startSeq, g.lastSeq}}
}

func (s *State) attributeTogether(a *awaiting) *journal.Failure {
	intent := a.intent
	f := &journal.Failure{Signal: a.end.Signal, Attribution: journal.Unattributed, Trial: intent.Trial, Regime: intent.Regime, Condition: intent.Condition, Profile: slices.Clone(intent.Profile)}
	var named []int
	if a.end.Core != nil {
		named = []int{*a.end.Core}
	} else {
		for _, seq := range a.cause {
			if m := s.mces[seq]; m != nil && m.BankType.CoreLocal() && !slices.Contains(named, m.Core) {
				named = append(named, m.Core)
			}
		}
	}
	if len(named) == 0 && (intent.Regime != machine.R7 || len(intent.Cores) <= 1) {
		if i, ok := SoleNonzero(intent.Profile); ok && i < len(s.cores) {
			named = []int{s.byID()[i].id}
		}
	}
	if len(named) == 1 {
		if i := s.index(named[0]); i >= 0 && i < len(intent.Profile) {
			f.Attribution = journal.Attributed
			f.Core = new(named[0])
			f.Offset = new(intent.Profile[i])
		}
	}
	return f
}

func (s *State) pendingDecision() (Action, bool) {
	a, ok := s.ordinaryDecision()
	if !ok {
		return a, false
	}
	return s.laterDecision(a), true
}

func (s *State) ordinaryDecision() (Action, bool) {
	if a, ok := s.r7Decision(); ok {
		return a, true
	}
	for _, c := range s.cores {
		if c.pending == 0 {
			continue
		}
		seq := c.pending
		failure := s.failureBySeq(seq)
		if failure == nil {
			return Action{}, false
		}
		return s.attributedDecision(c, failure.failure, seq)
	}
	return Action{}, false
}

func (s *State) attributedDecision(c *core, f *journal.Failure, seq int) (Action, bool) {
	if c.phase == journal.PhaseSearch {
		return Action{Kind: Decide, Payload: s.searchFailure(c.snapshot()), Cause: []int{seq}}, true
	}
	if f.Offset == nil {
		return Action{}, false
	}
	if *f.Offset == 0 {
		if failure := s.failureBySeq(seq); failure != nil {
			return s.atZero(*failure, failedAtZero(c.id), []int{seq})
		}
		return Action{Kind: Decide, Payload: failedAtZero(c.id), Cause: []int{seq}}, true
	}
	if s.hunt != nil && f.Condition == machine.Parked && s.hunt.end == nil {
		return Action{Kind: Decide, Payload: &journal.HuntEnd{Hunt: s.hunt.start.Hunt, Result: "direct", Cores: []int{c.id}, Groups: len(s.hunt.groups), Reason: fmt.Sprintf("attributed failure #%d", seq)}, Cause: []int{seq}}, true
	}
	deepening := f.Round > 0
	if intent := s.intents[f.Trial]; f.KnownFailure == 0 && intent != nil && intent.Round > 0 {
		deepening = true
	}
	if s.round != nil && deepening {
		return Action{Kind: Decide, Payload: &journal.DeepeningRound{Round: s.round.start.Round, Event: journal.CycleEnd, Reason: fmt.Sprintf("attributed failure #%d", seq)}, Cause: []int{seq}}, true
	}
	phase := journal.PhaseChecking
	if f.Condition == machine.Parked {
		phase = journal.PhaseHunt
	} else if deepening {
		phase = journal.PhaseDeepening
	}
	fail := *f.Offset
	if c.fail != nil {
		fail = max(fail, *c.fail)
	}
	to := backoffTarget(c, *f.Offset)
	pass, _ := keepPass(c.pass, fail)
	reason := fmt.Sprintf("attributed %s in %s %s trial %s; failure point %d", f.Signal, f.Condition, f.Regime, f.Trial, fail)
	if f.KnownFailure != 0 {
		reason += "; " + f.Reason
	}
	if to == c.offset {
		reason += "; already shallower"
	}
	cause := seq
	if s.hunt != nil && s.hunt.end != nil && s.hunt.end.Result == "direct" {
		cause = s.hunt.endSeq
		reason += s.hunt.pairedClause()
	}
	return Action{Kind: Decide, Payload: &journal.TunerDecision{Core: c.id, Phase: phase, Decision: journal.Backoff, FromOffset: c.offset, ToOffset: to, Pass: pass, FailurePoint: new(fail), Reason: reason}, Cause: []int{cause}}, true
}

// backoffTarget is where an attributed failure at offset failing steps core c back to: one count shallower than the
// failure, never deeper than where the core already is.
func backoffTarget(c *core, failing int) int { return max(c.offset, failing+1) }

// pendingRerun retires completed checks and retains their carried citations until
// a cycle or deepening decision consumes them. Fold calls it as evidence,
// profiles and commitments change, so replay does not depend on calls to Next.
func (s *State) pendingRerun() (trialClass, bool) {
	for len(s.obligations) > 0 {
		r := s.obligations[0]
		short := r.class.withDuration(s.durations.ShortTrialS)
		seqs := s.passSeqs(short, s.checking.profile, r.seq, rerunEvidence)
		if len(seqs) < s.n {
			return short, true
		}
		seqs = seqs[:s.n]
		if r.class.duration != s.durations.ShortTrialS {
			long := s.passSeqs(r.class, s.checking.profile, r.seq, rerunEvidence)
			if len(long) < 1 {
				return r.class, true
			}
			seqs = append(seqs, long[0])
		}
		for _, seq := range seqs {
			if _, carried := s.carriedSources[seq]; carried && !slices.Contains(s.rerunCauses, seq) {
				s.rerunCauses = append(s.rerunCauses, seq)
			}
		}
		s.obligations = s.obligations[1:]
	}
	return trialClass{}, false
}

func (s *State) rerunNext() (Action, bool) {
	k, pending := s.pendingRerun()
	if !pending {
		return Action{}, false
	}
	return s.rerunTrial(k), true
}

func (s *State) afterReruns(a Action) Action {
	if a.Kind != Decide || len(s.rerunCauses) == 0 {
		return a
	}
	reason := "; rerun checks passed" + s.carriedReason(s.rerunCauses)
	switch p := a.Payload.(type) {
	case *journal.CheckingCycle:
		p.Reason += reason
	case *journal.DeepeningRound:
		p.Reason += reason
	default:
		return a
	}
	a.Cause = append(a.Cause, s.rerunCauses...)
	return a
}

func (s *State) rerunTrial(k trialClass) Action {
	failure := s.obligations[0].seq
	for _, r := range s.obligations[1:] {
		if r.class == s.obligations[0].class && r.seq > failure {
			failure = r.seq
		}
	}
	t := Trial{Regime: k.regime, Workload: k.workload, Phase: journal.PhaseChecking, Condition: machine.Together, DurationS: k.duration, Rerun: true}
	target := s.classTargets[k.cores]
	if target.multi || len(target.cores) == 0 {
		t.Cores = slices.Clone(target.cores)
	} else if c := s.core(target.cores[0]); c != nil {
		t.Core, t.Offset = c.id, c.offset
	}
	class := k
	if k.workload == "" || len(target.cores) == 0 || !slices.IsSorted(target.cores) || !target.multi && s.core(target.cores[0]) == nil {
		// The built shape no longer matches the selected class (legacy journals), so the journal will record its own.
		class = s.shapeClass(t)
	}
	sel := s.rerunRequirement(class)
	if retry, ok := s.retryFor(sel); ok {
		return s.runTrial(retry, sel, []int{failure})
	}
	return s.runTrial(t, sel, []int{failure})
}
