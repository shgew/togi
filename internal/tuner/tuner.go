// Package tuner is the pure decision engine: a fold of journal events that proposes the next decision or trial.
package tuner

import (
	"cmp"
	"fmt"
	"maps"
	"slices"

	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

// Ruleset must be bumped for changes to steps, offset range, phases, regimes, evidence, hunts, refinement, tiers or backoffs; this is breaking.
const Ruleset = 4

type ActionKind int

const (
	Decide ActionKind = iota
	RunTrial
	ReadRanking
)

type Action struct {
	Kind    ActionKind
	Payload journal.Payload
	Trial   Trial
	Cause   []int
}

type Trial struct {
	Core, Offset      int
	Regime            machine.Regime
	Phase             journal.Phase
	Condition         machine.Condition
	Cores             []int
	Rotation          int
	Retry             bool
	Workload          string
	DurationS         int
	Profile           []int
	Hunt, Mask, Round int
	Rerun             bool
}

type awaiting struct {
	intent *journal.TrialIntent
	end    *journal.TrialEnd
	seq    int
	cause  []int
}

type core struct {
	id             int
	phase          journal.Phase
	offset         int
	baseline       int
	pass, fail     *int
	check          bool
	checkWorkloads [2]string
	phaseSeq       int
	checks         int
	queued         string
	queueSeq       int
	pending        int
	zeroSeq        int
	lastSeq        int
	decisionSeq    int
	stepR1         bool
	stepSeqs       []int
}

type pendingFailure struct {
	seq     int
	failure *journal.Failure
	profile []int
	class   trialClass
}

type rerun struct {
	class trialClass
	seq   int
}

type qualified struct {
	profile []int
	seq     int
	allDone bool
}

type State struct {
	cores                       []*core
	sortedCores                 []*core
	indexByID                   map[int]int
	cursor                      int
	retry                       *Trial
	intents                     map[string]*journal.TrialIntent
	intentSeq                   map[int]string
	signalled                   map[string]bool
	awaiting                    *awaiting
	mces                        map[int]*journal.MCE
	steps                       []machine.Regime
	durations                   config.Durations
	evidence                    config.Evidence
	n                           int
	ccd                         map[int]int
	parts                       [][]int
	guard                       guard
	ledger                      map[trialClass][]entry
	idle                        []entry
	failures                    []entry
	marks                       []journal.JointMarkState
	nextMark                    int
	recent                      []int
	queue                       []pendingFailure
	pendingFailures             []pendingFailure
	failureIndex                map[int]int
	obligations                 []rerun
	hunt                        *hunt
	nextHunt                    int
	round                       *round
	nextRound                   int
	ranking                     []int
	rankingSeq, lastPlanSeq     int
	qualified                   []qualified
	firstProfileSeq             int
	lastDeepenSeq, tierClockSeq int
	tierClockDirty              bool
	bestProfile                 []int
	bestDirty                   bool
	warning                     *journal.TunerWarning
	warningSeq                  int
	thermal                     *journal.DeadEnd
	thermalSeq                  int
	tier                        journal.Tier
	tierSeq, tierCause          int
	projectionDirty             bool
	projectedGuard              *journal.GuardState
}

func New() *State {
	c := config.Default()
	return &State{cursor: -1, intents: map[string]*journal.TrialIntent{}, intentSeq: map[int]string{}, signalled: map[string]bool{}, mces: map[int]*journal.MCE{}, ledger: map[trialClass][]entry{}, failureIndex: map[int]int{}, steps: c.Guard.Rotation, durations: c.Durations, evidence: c.Evidence, n: c.Evidence.Starts(), tier: journal.TierNone, projectionDirty: true, bestDirty: true}
}

func partition(cores []machine.CoreInfo) (map[int]int, [][]int) {
	ccd := map[int]int{}
	groups := map[int][]int{}
	all := make([]int, 0, len(cores))
	for _, c := range cores {
		ccd[c.Core] = c.CCD
		groups[c.CCD] = append(groups[c.CCD], c.Core)
		all = append(all, c.Core)
	}
	slices.Sort(all)
	if len(groups) == 1 {
		return ccd, [][]int{all}
	}
	var parts [][]int
	for _, id := range slices.Sorted(maps.Keys(groups)) {
		parts = append(parts, slices.Sorted(slices.Values(groups[id])))
	}
	return ccd, append(parts, all)
}

func (s *State) core(id int) *core {
	if i, ok := s.indexByID[id]; ok {
		return s.sortedCores[i]
	}
	return nil
}

func (s *State) byID() []*core {
	return s.sortedCores
}

func (s *State) offsets() []int {
	out := make([]int, len(s.cores))
	for i, c := range s.byID() {
		out[i] = c.offset
	}
	return out
}

func (s *State) ids() []int {
	out := make([]int, len(s.cores))
	for i, c := range s.byID() {
		out[i] = c.id
	}
	return out
}

func (s *State) index(id int) int {
	if i, ok := s.indexByID[id]; ok {
		return i
	}
	return -1
}

func (s *State) Fold(e journal.Event) {
	switch p := e.Data.(type) {
	case *journal.SessionStart:
		s.cores = nil
		for _, id := range machine.Order(p.Cores) {
			s.cores = append(s.cores, &core{id: id})
		}
		s.ccd, s.parts = partition(p.Cores)
		s.sortedCores = slices.Clone(s.cores)
		slices.SortFunc(s.sortedCores, func(a, b *core) int { return cmp.Compare(a.id, b.id) })
		s.indexByID = make(map[int]int, len(s.cores))
		for i, c := range s.sortedCores {
			s.indexByID[c.id] = i
		}
		s.bestDirty = true
	case *journal.ConfigLoaded:
		s.steps = slices.Clone(p.Config.Guard.Rotation)
		s.durations = p.Config.Durations
		s.evidence = p.Config.Evidence
		s.n = s.evidence.Starts()
		s.projectionDirty = true
	case *journal.SessionBaseline:
		for i, c := range s.byID() {
			if i < len(p.Offsets) {
				c.baseline = p.Offsets[i]
			}
		}
	case *journal.CommandReset:
		if p.Core != nil {
			if c := s.core(*p.Core); c != nil {
				c.queued, c.queueSeq = queuedReset, e.Seq
			}
		}
	case *journal.CorePhase:
		if c := s.core(p.Core); c != nil {
			if c.queued == queuedReset && s.hunt != nil && s.hunt.end != nil && slices.Contains(s.hunt.end.Cores, c.id) {
				s.hunt = nil
			}
			if len(p.ClearedJoint) > 0 {
				s.marks = slices.DeleteFunc(s.marks, func(m journal.JointMarkState) bool { return slices.Contains(p.ClearedJoint, m.Mark) })
			}
			if p.FailedMark != nil && (c.fail == nil || *p.FailedMark > *c.fail) {
				s.recent = []int{c.id}
			}
			c.phase, c.offset, c.pass, c.fail = p.To, p.Offset, p.Pass, p.FailedMark
			c.check = p.CheckEdge
			c.checkWorkloads = [2]string{}
			if len(p.Workloads) == 2 {
				copy(c.checkWorkloads[:], p.Workloads)
			}
			if p.CheckEdge {
				c.checks++
			}
			c.phaseSeq = e.Seq
			c.queued = ""
			if p.FailedMark != nil && *p.FailedMark == 0 {
				c.zeroSeq = e.Seq
			}
			s.decided(c, e.Seq)
		}
	case *journal.TunerDecision:
		if c := s.core(p.Core); c != nil {
			if p.FailedMark != nil && (c.fail == nil || *p.FailedMark > *c.fail) {
				s.recent = []int{c.id}
			}
			c.offset, c.pass, c.fail = p.ToOffset, p.Pass, p.FailedMark
			c.check = p.Decision == journal.CheckEdge
			if c.check {
				c.checks++
				c.phaseSeq = e.Seq
				if len(p.Workloads) == 2 {
					copy(c.checkWorkloads[:], p.Workloads)
				}
			}
			s.decided(c, e.Seq)
			if p.Decision == journal.Backoff && p.Phase != journal.PhaseSearch && p.ToOffset != p.FromOffset {
				s.queueRerun(e)
			}
			if s.hunt != nil && s.hunt.end != nil && p.Phase == journal.PhaseHunt && len(e.Cause) > 0 && (e.Cause[0] == s.hunt.endSeq || e.Cause[0] == s.hunt.markSeq) {
				s.hunt = nil
			}
		}
	case *journal.TrialIntent:
		s.intents[p.Trial] = p
		s.intentSeq[e.Seq] = p.Trial
		s.retry = nil
		if p.Condition == machine.Isolated && p.Core != nil {
			s.cursor = slices.IndexFunc(s.cores, func(c *core) bool { return c.id == *p.Core })
		}
	case *journal.TrialProgress:
		if p.Signal != "" {
			s.signalled[p.Trial] = true
		}
	case *journal.TrialEnd:
		s.foldTrialEnd(e, p)
	case *journal.MCE:
		s.mces[e.Seq] = p
	case *journal.Failure:
		s.foldFailure(e, p)
	case *journal.DeadEnd:
		if p.Condition == journal.DeadEndThermalTrip {
			s.thermal = nil
		}
		if p.Condition == journal.DeadEndFailureAtZero && p.Core != nil {
			if c := s.core(*p.Core); c != nil {
				c.fail = new(0)
				c.zeroSeq = e.Seq
				c.pending = 0
			}
		}
	case *journal.CrashDetected:
		if p.ResetReason == machine.ResetThermalTrip && !p.Inconclusive {
			evidence := false
			if p.InFlight != nil {
				evidence = s.signalled[s.intentSeq[*p.InFlight]]
			}
			for _, cause := range e.Cause {
				if s.mces[cause] != nil {
					evidence = true
					break
				}
			}
			if !evidence {
				s.thermal = &journal.DeadEnd{Condition: journal.DeadEndThermalTrip, Detail: fmt.Sprintf("the machine reset on a thermal trip (%s); check cooling before tuning again", p.ResetReasonRaw)}
				s.thermalSeq = e.Seq
			}
		}
	case *journal.ProfileChange:
		if s.firstProfileSeq == 0 {
			s.firstProfileSeq = e.Seq
		}
		if len(s.guard.profile) == len(p.To) {
			for i, x := range p.To {
				if x < s.guard.profile[i] {
					s.lastDeepenSeq = e.Seq
					break
				}
			}
		}
		s.guard.profile = slices.Clone(p.To)
		s.guard.profileSeq = e.Seq
		s.guard.lastSeq = e.Seq
		s.guard.tctlMax = nil
		s.guard.tctlSeq = 0
		s.tierCause = e.Seq
		s.projectionDirty = true
		s.recomputeTierClock()
	case *journal.GuardRotation:
		s.foldRotation(e, p)
	case *journal.TierChange:
		s.tier, s.tierSeq = p.To, e.Seq
	case *journal.HostRanking:
		s.ranking = slices.Clone(p.Ranking)
		s.rankingSeq = e.Seq
		s.bestDirty = true
	case *journal.HuntStart:
		s.openHunt(e, p)
	case *journal.HuntMask:
		s.recordMask(e, p)
	case *journal.HuntEnd:
		s.endHunt(e, p)
	case *journal.HuntSkipped:
		if len(s.queue) > 0 && s.queue[0].seq == p.Failure {
			s.queue = s.queue[1:]
		}
	case *journal.MarkJoint:
		s.marks = append(s.marks, journal.JointMarkState{Mark: p.Mark, Members: slices.Clone(p.Members), Fallback: p.Fallback, Hunt: p.Hunt, Seq: e.Seq})
		s.bestDirty = true
		s.nextMark = max(s.nextMark, p.Mark)
		s.recent = nil
		for _, m := range p.Members {
			s.recent = append(s.recent, m.Core)
		}
		if s.hunt != nil && s.hunt.end != nil && s.hunt.start.Hunt == p.Hunt {
			s.hunt.markSeq = e.Seq
			if len(s.guard.profile) == len(s.cores) {
				for _, m := range p.Members {
					if s.guard.profile[s.index(m.Core)] > m.Offset {
						s.hunt = nil
						break
					}
				}
			}
		}
	case *journal.RefineRound:
		s.foldRound(e, p)
	case *journal.TunerWarning:
		s.warning = nil
	}
}

func (s *State) decided(c *core, seq int) {
	c.stepR1 = false
	c.stepSeqs = nil
	c.pending = 0
	c.lastSeq = seq
	c.decisionSeq = seq
	s.tierCause = seq
	s.projectionDirty = true
	s.bestDirty = true
	if s.retry != nil && s.retry.Condition == machine.Isolated && s.retry.Core == c.id {
		s.retry = nil
	}
}

func (s *State) foldTrialEnd(e journal.Event, p *journal.TrialEnd) {
	intent := s.intents[p.Trial]
	if intent == nil {
		return
	}
	if s.thermal != nil && p.Outcome == journal.OutcomeFailure && p.Signal != machine.Crash {
		s.thermal = nil
	}
	s.recordEvidence(e, intent, p)
	if intent.Condition != machine.Isolated {
		if p.Outcome == journal.OutcomePass && intent.Condition == machine.Resident {
			s.guard.lastSeq = e.Seq
			s.tierCause = e.Seq
			if p.TctlMaxC != nil && (s.guard.tctlMax == nil || *p.TctlMaxC > *s.guard.tctlMax) {
				s.guard.tctlMax = new(*p.TctlMaxC)
				s.guard.tctlSeq = e.Seq
			}
		}
		if p.Outcome == journal.OutcomeInconclusive {
			t := trialFromIntent(intent)
			t.Retry = true
			s.retry = &t
		}
		if p.Outcome == journal.OutcomeFailure {
			s.awaiting = &awaiting{intent, p, e.Seq, e.Cause}
		}
		return
	}
	if intent.Core == nil || intent.Offset == nil {
		return
	}
	c := s.core(*intent.Core)
	if c == nil {
		return
	}
	c.lastSeq = e.Seq
	switch p.Outcome {
	case journal.OutcomePass:
		if c.phase == journal.PhaseSearch && *intent.Offset == c.offset && intent.Phase == journal.PhaseSearch && !c.check {
			if intent.Regime == machine.R1 {
				c.stepR1 = true
			}
			c.stepSeqs = append(c.stepSeqs, e.Seq)
		}
	case journal.OutcomeInconclusive:
		t := trialFromIntent(intent)
		t.Retry = true
		s.retry = &t
	case journal.OutcomeFailure:
		s.awaiting = &awaiting{intent, p, e.Seq, e.Cause}
	}
}

func trialFromIntent(p *journal.TrialIntent) Trial {
	t := Trial{Regime: p.Regime, Phase: p.Phase, Condition: p.Condition, Cores: slices.Clone(p.Cores), Workload: p.Workload, DurationS: p.DurationS, Profile: slices.Clone(p.Profile), Rotation: p.Rotation, Hunt: p.Hunt, Mask: p.Mask, Round: p.Round, Rerun: p.Rerun}
	if p.Core != nil {
		t.Core = *p.Core
	}
	if p.Offset != nil {
		t.Offset = *p.Offset
	}
	return t
}

func (s *State) foldFailure(e journal.Event, p *journal.Failure) {
	if a := s.awaiting; a != nil && a.intent.Trial == p.Trial {
		s.awaiting = nil
	}
	profile := slices.Clone(p.Profile)
	if len(profile) != len(s.cores) {
		if intent := s.intents[p.Trial]; intent != nil {
			profile = slices.Clone(intent.Profile)
		}
	}
	failure := pendingFailure{seq: e.Seq, failure: p, profile: profile}
	if intent := s.intents[p.Trial]; intent != nil {
		failure.class = classOf(intent)
	} else if p.Trial == "" && (p.Condition == machine.Resident || p.Condition == machine.Masked) {
		failure.class = trialClass{machine.R6, machine.Workloads(machine.R6)[0].ID, fmt.Sprint(s.ids()), s.durations.GuardIdleS}
	}
	s.failureIndex[e.Seq] = len(s.pendingFailures)
	s.pendingFailures = append(s.pendingFailures, failure)
	if atLeastDeep(profile, s.guard.profile) {
		s.tierClockSeq = max(s.tierClockSeq, e.Seq)
	}
	if p.Attribution == journal.Attributed && p.Core != nil {
		if c := s.core(*p.Core); c != nil {
			c.pending = e.Seq
		}
	}
	if p.Attribution == journal.Unattributed && (p.Condition == machine.Resident || p.Condition == machine.Masked) {
		if p.Condition != machine.Masked || s.intents[p.Trial] == nil || p.Trial == "" {
			s.queue = append(s.queue, failure)
		}
	}
	if p.Trial == "" && (p.Condition == machine.Resident || p.Condition == machine.Masked) && p.Attribution == journal.Unattributed {
		s.recordIdle(e, p)
	}
	s.projectionDirty = true
}

func (s *State) Attribution() (Action, bool) {
	a := s.awaiting
	if a == nil {
		return Action{}, false
	}
	var f *journal.Failure
	switch a.intent.Condition {
	case machine.Isolated:
		f = attributeIsolated(a.intent, a.end.Signal)
	case machine.Resident, machine.Masked:
		f = s.attributeResident(a)
	}
	if f == nil {
		return Action{}, false
	}
	return Action{Kind: Decide, Payload: f, Cause: append([]int{a.seq}, a.cause...)}, true
}

func (s *State) Next() Action {
	if len(s.cores) == 0 {
		panic("tuner: no session")
	}
	for _, c := range s.cores {
		if c.phase == "" {
			panic(fmt.Sprintf("tuner: core %d has no phase", c.id))
		}
	}
	if a, ok := s.Attribution(); ok {
		return a
	}
	if s.warning != nil {
		return Action{Kind: Decide, Payload: s.warning, Cause: []int{s.warningSeq}}
	}
	if s.thermal != nil {
		return Action{Kind: Decide, Payload: s.thermal, Cause: []int{s.thermalSeq}}
	}
	if a, ok := s.queuedReset(); ok {
		return a
	}
	for _, c := range s.cores {
		if c.fail != nil && *c.fail == 0 {
			return Action{Kind: Decide, Payload: &journal.DeadEnd{Condition: journal.DeadEndFailureAtZero, Core: new(c.id), Detail: fmt.Sprintf("core %02d has a failed mark at CO 0; only reset can clear it", c.id)}, Cause: []int{c.zeroSeq}}
		}
	}
	if len(s.queue) > 0 && allZero(s.queue[0].profile) {
		return Action{Kind: Decide, Payload: &journal.DeadEnd{Condition: journal.DeadEndFailureAtZero, Detail: "unattributed failure with every core at CO 0; the instability is not caused by Curve Optimizer"}, Cause: []int{s.queue[0].seq}}
	}
	if a, ok := s.pendingDecision(); ok {
		return a
	}
	if s.hunt != nil {
		if a, ok := s.huntNext(); ok {
			return a
		}
	}
	if len(s.queue) > 0 && s.round != nil {
		return Action{Kind: Decide, Payload: &journal.RefineRound{Round: s.round.start.Round, Event: journal.RotationEnd, Reason: "a failure needs a hunt"}, Cause: []int{s.queue[0].seq}}
	}
	if s.round != nil {
		if a, ok := s.roundMoves(); ok {
			return a
		}
	}
	if a, ok := s.phaseNext(); ok {
		return a
	}
	if !s.anySearch() && !slices.Equal(s.guard.profile, s.offsets()) {
		return s.profileNext()
	}
	if a, ok := s.tierNext(); ok {
		return a
	}
	if s.anySearch() {
		if a, ok := s.perCore(); ok {
			return a
		}
	}
	if len(s.queue) > 0 {
		if s.rankingSeq <= s.lastPlanSeq {
			return Action{Kind: ReadRanking}
		}
		return s.huntStartNext()
	}
	if a, ok := s.rerunNext(); ok {
		return a
	}
	if s.round != nil {
		return s.roundCheck()
	}
	if s.guard.open {
		return s.rotationNext()
	}
	if s.refineDue() {
		if s.rankingSeq <= s.lastPlanSeq {
			return Action{Kind: ReadRanking}
		}
		return s.roundStart()
	}
	return Action{Kind: Decide, Payload: &journal.GuardRotation{Rotation: s.guard.rotation + 1, Event: journal.RotationStart, Steps: slices.Clone(s.steps)}, Cause: []int{s.guard.lastSeq}}
}

func (s *State) anySearch() bool {
	return slices.ContainsFunc(s.cores, func(c *core) bool { return c.phase == journal.PhaseSearch })
}

func (s *State) profileNext() Action {
	var causes []int
	for _, c := range s.cores {
		if c.decisionSeq > s.guard.profileSeq {
			causes = append(causes, c.decisionSeq)
		}
	}
	slices.Sort(causes)
	return Action{Kind: Decide, Payload: &journal.ProfileChange{From: slices.Clone(s.guard.profile), To: s.offsets()}, Cause: causes}
}

func (s *State) phaseNext() (Action, bool) {
	p := s.offsets()
	for _, c := range s.cores {
		if c.phase == journal.PhaseSearch {
			continue
		}
		reason, done := s.done(c, p)
		want := journal.PhaseResident
		if done {
			want = journal.PhaseDone
		} else {
			reason = "one count deeper reaches no mark"
		}
		if c.phase != want {
			return Action{Kind: Decide, Payload: &journal.CorePhase{Core: c.id, From: c.phase, To: want, Offset: c.offset, Pass: c.pass, FailedMark: c.fail, Reason: reason}, Cause: []int{c.decisionSeq}}, true
		}
	}
	return Action{}, false
}

func (s *State) Profile() []int  { return slices.Clone(s.guard.profile) }
func (s *State) ProfileSeq() int { return s.guard.profileSeq }
func (s *State) QualifiedRotations() int {
	n := 0
	for _, q := range s.qualified {
		if q.seq > s.lastDeepenSeq && q.allDone {
			n++
		}
	}
	return n
}

func (s *State) Project(st *journal.State) {
	st.Phase = string(journal.PhaseGuard)
	switch {
	case s.anySearch():
		st.Phase = string(journal.PhaseSearch)
	case s.hunt != nil:
		st.Phase = string(journal.PhaseHunt)
	case s.round != nil:
		st.Phase = string(journal.PhaseRefine)
	}
	for i := range st.Cores {
		c := s.core(st.Cores[i].Core)
		if c == nil {
			continue
		}
		x := &st.Cores[i]
		x.Offset = c.offset
		x.Phase = c.phase
		x.Pass = c.pass
		x.FailedMark = c.fail
		x.Queued = c.queued
		x.JointMarks = nil
		for _, m := range s.marks {
			for _, member := range m.Members {
				if member.Core == c.id {
					x.JointMarks = append(x.JointMarks, m.Mark)
				}
			}
		}
	}
	st.JointMarks = slices.Clone(s.marks)
	st.Hunt = s.projectHunt()
	st.Refine = s.projectRound()
	st.Guard = s.projectGuard()
	st.Tier, st.TierSeq = s.tier, s.tierSeq
}
