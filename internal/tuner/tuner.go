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

// Ruleset must be bumped for changes to steps, offset range, phases, regimes, evidence, hunts, deepening or backoffs; this is breaking.
const Ruleset = 10

const EvidenceEpoch = 1

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
	Core, Offset       int
	Regime             machine.Regime
	Phase              journal.Phase
	Condition          machine.Condition
	Cores              []int
	Cycle              int
	Retry              bool
	Workload           string
	DurationS          int
	Profile            []int
	Hunt, Group, Round int
	Rerun              bool
	Step               int
	Requirement        ScheduledRequirement
}

func (t Trial) Complete(index int, profile []int) (*journal.TrialIntent, machine.Workload, error) {
	w := machine.PickWorkload(t.Regime, index)
	if t.Workload != "" {
		i := slices.IndexFunc(machine.Workloads(t.Regime), func(w machine.Workload) bool { return w.ID == t.Workload })
		if i < 0 {
			return nil, machine.Workload{}, fmt.Errorf("trial workload %s: not a %s workload", t.Workload, t.Regime)
		}
		w = machine.Workloads(t.Regime)[i]
	}
	p := &journal.TrialIntent{
		Regime: t.Regime, Workload: w.ID, DurationS: t.DurationS,
		Condition: t.Condition, Phase: t.Phase, Retry: t.Retry, Cycle: t.Cycle, Step: t.Step,
		Profile: profile, Hunt: t.Hunt, Group: t.Group, Round: t.Round, Rerun: t.Rerun,
	}
	if len(t.Cores) > 0 {
		p.Cores = t.Cores
	} else {
		p.Core, p.Offset = new(t.Core), new(t.Offset)
	}
	return p, w, nil
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
	stepPasses     []stepPass
	workloadIndex  map[machine.Regime]int
}

// stepPass is a search step pass and the backend store path it ran under; currentStep derives the core's stepR1 and
// stepSeqs from those whose path is still current.
type stepPass struct {
	seq     int
	r1      bool
	backend machine.Backend
	path    string
}

type pendingFailure struct {
	seq     int
	failure *journal.Failure
	profile []int
	class   trialClass
	carried bool
	// loadBackoffs are the voltage-targeted backoffs of the failed load since its latest passing trial, as folded
	// before this failure.
	loadBackoffs []loadBackoff
}

type rerun struct {
	class trialClass
	seq   int
}

type passedFullCycle struct {
	profile    []int
	seq        int
	cycle      int
	allAtLimit bool
}

type State struct {
	cores                   []*core
	sortedCores             []*core
	indexByID               map[int]int
	cursor                  int
	retry                   *savedRetry
	intents                 map[string]*journal.TrialIntent
	scheduled               map[string]scheduledTrial
	intentSeq               map[int]string
	signalled               map[string]bool
	flight                  *journal.TrialIntent
	flightCrashed           bool // crash.detected named the boot of flight; recovery has yet to close it
	awaiting                *awaiting
	mces                    map[int]*journal.MCE
	steps                   []machine.Regime
	durations               journal.ConfigDurations
	evidence                journal.ConfigEvidence
	n                       int
	backends                journal.ConfigBackends
	ccd                     map[int]int
	parts                   [][]int
	checking                checking
	ledger                  map[trialClass][]entry
	classTargets            map[string]classTarget
	idle                    []entry
	carriedSources          map[int]string
	failures                []entry
	combinations            []journal.CombinationState
	nextCombination         int
	recent                  []int
	queue                   []pendingFailure
	pendingFailures         []pendingFailure
	failureIndex            map[int]int
	failureGroups           map[int][]int
	obligations             []rerun
	rerunCauses             []int
	hunt                    *hunt
	nextHunt                int
	resetSeq                int
	round                   *round
	nextRound               int
	ranking                 []int
	rankingSeq, lastPlanSeq int
	passedFullCycles        []passedFullCycle
	lastDeepenSeq           int
	bestProfile             []int
	bestDirty               bool
	warning                 *journal.TunerWarning
	warningSeq              int
	projectionDirty         bool
	projectedChecking       *journal.CheckingState
	exposure                map[trialClass]int
	exposureProfileSeq      int
	projectedHunt           *journal.HuntState
	r7Handled               map[int]map[int]bool
	r7Measurements          []entry
	located                 map[int]locatedHunt
	zeroReruns              map[int]*zeroRerun
	zeroTrials              map[string]int
	loads                   map[r7Load][]loadBackoff

	// Indexes and memos over the folded events, so a query costs what changed recently rather than the whole
	// history. Each is a function of the folded events alone.
	allKey        string
	classFailures map[trialClass][]int // per class, the ledger positions of its failures, in seq order
	measurements  map[measurementKey][]int
	ccdKeys       map[int]string
	derivedBySeq  map[int]*derived
	failurePos    map[int]failurePos
	// gen counts folded events; memos keyed by it hold only until the next fold.
	gen                int
	reqEpoch, roundGen int
	reqByStep          map[int][]requirement
	cycleCheckingEpoch int
	cycleMemo          map[trialClass]int
	// checkingEpoch changes with every event that can change the checking cycle, its partial chains, the checking
	// profile, core offsets or durations; evidenceEpoch with every change to idle failures, backends or deleted
	// evidence. A new ledger entry drops only its own class from cycleMemo.
	checkingEpoch, evidenceEpoch int
	cycleEvidenceEpoch           int
	// limitEpoch changes with every event that can change a core's phase, offset, pass, failure point or the
	// combinations; phasesSettled holds while phaseNext found nothing to decide at phasesEpoch.
	limitEpoch, phasesEpoch int
	phasesSettled           bool
	roundMemo               bool
	roundChecksMemo         []requirement
	roundSourcesMemo        []int
	r7Epoch, r7OpenEpoch    int
	r7Open                  []int
	r7OpenBuilt             bool
}

func New() *State {
	c := config.Default()
	return &State{cursor: -1, intents: map[string]*journal.TrialIntent{}, scheduled: map[string]scheduledTrial{}, intentSeq: map[int]string{}, signalled: map[string]bool{}, mces: map[int]*journal.MCE{}, ledger: map[trialClass][]entry{}, carriedSources: map[int]string{}, failureIndex: map[int]int{}, steps: c.Checking.Cycle, durations: journal.ConfigDurations(c.Durations), evidence: journal.ConfigEvidence(c.Evidence), n: c.Evidence.Trials(), projectionDirty: true, bestDirty: true}
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
	s.gen++
	switch e.Data.(type) {
	case *journal.SessionStart, *journal.ConfigLoaded, *journal.CorePhase, *journal.TunerDecision, *journal.ProfileChange,
		*journal.CheckingCycle, *journal.CheckingStep, *journal.CheckingChain:
		s.checkingEpoch++
	}
	switch e.Data.(type) {
	case *journal.SessionStart, *journal.CorePhase, *journal.TunerDecision, *journal.DeadEnd, *journal.Combination:
		s.limitEpoch++
	}
	switch p := e.Data.(type) {
	case *journal.SessionStart:
		s.exposure, s.derivedBySeq, s.ccdKeys = nil, nil, nil
		s.r7Epoch++
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
		s.allKey = coresKey(s.ids())
		s.indexClassTargets()
		s.bestDirty = true
		s.projectionDirty = true
	case *journal.ConfigLoaded:
		s.steps = slices.Clone(p.Config.Checking.Cycle)
		s.durations = p.Config.Durations
		s.evidence = p.Config.Evidence
		s.backends = p.Config.Backends
		s.evidenceEpoch++
		s.exposure, s.derivedBySeq = nil, nil
		s.r7Epoch++
		for _, c := range s.cores {
			s.currentStep(c)
		}
		s.n = config.Evidence(s.evidence).Trials()
		s.pendingRerun()
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
				s.resetSeq = e.Seq
				s.projectionDirty = true
				s.resetEvidence(*p.Core)
				c.queued, c.queueSeq = queuedReset, e.Seq
				s.retry = nil // a pending reset supersedes every saved retry
			}
		}
	case *journal.CorePhase:
		if c := s.core(p.Core); c != nil {
			s.resetHuntCore(c)
			if len(p.ClearedCombination) > 0 {
				s.combinations = slices.DeleteFunc(s.combinations, func(m journal.CombinationState) bool { return slices.Contains(p.ClearedCombination, m.Combination) })
			}
			if p.FailurePoint != nil && (c.fail == nil || *p.FailurePoint > *c.fail) {
				s.recent = []int{c.id}
			}
			c.phase, c.offset, c.pass, c.fail = p.To, p.Offset, p.Pass, p.FailurePoint
			c.check = p.CheckSoloLimit
			c.checkWorkloads = [2]string{}
			if len(p.Workloads) == 2 {
				copy(c.checkWorkloads[:], p.Workloads)
			}
			if p.CheckSoloLimit {
				c.checks++
			}
			c.phaseSeq = e.Seq
			c.queued = ""
			if p.FailurePoint != nil && *p.FailurePoint == 0 {
				c.zeroSeq = e.Seq
			}
			s.decided(c, e.Seq)
		}
	case *journal.TunerDecision:
		if p.Decision == journal.Backoff {
			s.recordLoadBackoff(e)
			s.consumeR7(e, p.Core)
		}
		if c := s.core(p.Core); c != nil {
			if p.FailurePoint != nil && (c.fail == nil || *p.FailurePoint > *c.fail) {
				s.recent = []int{c.id}
			}
			c.offset, c.pass, c.fail = p.ToOffset, p.Pass, p.FailurePoint
			c.check = p.Decision == journal.CheckSoloLimit
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
				s.pendingRerun()
			}
			s.commitHuntDecision(e, p)
		}
	case *journal.TrialIntent:
		s.scheduled[p.Trial] = s.scheduledFor(p, e.Cause)
		s.flight, s.flightCrashed = p, false
		s.intents[p.Trial] = p
		s.intentSeq[e.Seq] = p.Trial
		s.recordCheckingTrial(p)
		s.recordZeroRerun(e, p)
		s.retry = nil
		if p.Condition == machine.Alone && p.Core != nil {
			s.cursor = slices.IndexFunc(s.cores, func(c *core) bool { return c.id == *p.Core })
		}
	case *journal.TrialProgress:
		if p.Signal != "" {
			s.signalled[p.Trial] = true
		}
	case *journal.TrialEnd:
		s.recordTrialHistory(p)
		if s.flight != nil && s.flight.Trial == p.Trial {
			s.flight, s.flightCrashed = nil, false
		}
		s.foldTrialEnd(e, p)
		s.endZeroRerun(e, p)
		s.pendingRerun()
	case *journal.MCE:
		s.mces[e.Seq] = p
	case *journal.Failure:
		s.foldFailure(e, p)
	case *journal.TrialCarried:
		s.recordCarried(e, p)
		s.pendingRerun()
	case *journal.FailureCarried:
		s.recordIdle(e, &p.Failure)
		if _, recorded := s.carriedSources[e.Seq]; recorded {
			s.rememberCarriedFailure(e.Seq, &p.Failure, trialClass{machine.R6, machine.Workloads(machine.R6)[0].ID, coresKey(s.ids()), s.durations.CheckingIdleS})
		}
	case *journal.DeadEnd:
		if p.Condition == journal.DeadEndFailureAtZero && p.Core != nil {
			if c := s.core(*p.Core); c != nil {
				c.fail = new(0)
				c.zeroSeq = e.Seq
				c.pending = 0
			}
		}
	case *journal.CrashDetected:
		// The crashed trial stays in flight until recovery closes it with its trial.end, as the session's fold keeps it.
		s.flightCrashed = s.flight != nil
	case *journal.ProfileChange:
		if len(s.checking.profile) == len(p.To) {
			for i, x := range p.To {
				if x < s.checking.profile[i] {
					s.lastDeepenSeq = e.Seq
					break
				}
			}
		}
		s.checking.profile = slices.Clone(p.To)
		s.pendingRerun()
		s.checking.profileSeq = e.Seq
		s.checking.lastSeq = e.Seq
		s.projectionDirty = true
	case *journal.CheckingCycle:
		s.foldCycle(e, p)
		s.rerunCauses = nil
	case *journal.CheckingStep:
		s.recordCheckingStep(e, p)
	case *journal.CheckingChain:
		s.recordCheckingChain(e, p)
	case *journal.HostRanking:
		s.ranking = slices.Clone(p.Ranking)
		s.rankingSeq = e.Seq
		s.bestDirty = true
	case *journal.HuntStart:
		s.openHunt(e, p)
	case *journal.HuntGroup:
		s.recordGroup(e, p)
	case *journal.HuntEnd:
		s.endHunt(e, p)
	case *journal.HuntSkipped:
		s.skipHunt(p)
	case *journal.Combination:
		s.combinations = append(s.combinations, journal.CombinationState{Combination: p.Combination, Members: slices.Clone(p.Members), Fallback: p.Fallback, Hunt: p.Hunt, Seq: e.Seq})
		s.bestDirty = true
		s.nextCombination = max(s.nextCombination, p.Combination)
		s.recent = nil
		for _, m := range p.Members {
			s.recent = append(s.recent, m.Core)
		}
		s.recordHuntCombination(e, p)
	case *journal.DeepeningRound:
		s.foldRound(e, p)
		s.rerunCauses = nil
	case *journal.TunerWarning:
		s.warning = nil
	}
	s.dropEndedRetry()
}

func (s *State) decided(c *core, seq int) {
	c.stepR1 = false
	c.stepSeqs = nil
	c.stepPasses = nil
	c.pending = 0
	c.lastSeq = seq
	c.decisionSeq = seq
	s.projectionDirty = true
	s.bestDirty = true
	if s.retry != nil && s.retry.trial.Condition == machine.Alone && s.retry.trial.Core == c.id {
		s.retry = nil
	}
}

func (s *State) foldTrialEnd(e journal.Event, p *journal.TrialEnd) {
	intent := s.intents[p.Trial]
	if intent == nil {
		return
	}
	s.recordEvidence(e, intent, p)
	s.recordR7Measurement(e.Seq, intent, p)
	if intent.Core != nil && (p.Outcome == journal.OutcomePass || p.Outcome == journal.OutcomeFailure) {
		if c := s.core(*intent.Core); c != nil {
			if c.workloadIndex == nil {
				c.workloadIndex = map[machine.Regime]int{}
			}
			c.workloadIndex[intent.Regime]++
		}
	}
	if intent.Condition != machine.Alone {
		if p.Outcome == journal.OutcomePass && intent.Condition == machine.Together {
			s.checking.lastSeq = e.Seq
		}
		if p.Outcome == journal.OutcomeInconclusive {
			s.saveRetry(intent)
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
			w, _ := machine.WorkloadByID(intent.Workload)
			c.stepPasses = append(c.stepPasses, stepPass{e.Seq, intent.Regime == machine.R1, w.Backend, s.backends.Path(w.Backend)})
			s.currentStep(c)
		}
	case journal.OutcomeInconclusive:
		s.saveRetry(intent)
	case journal.OutcomeFailure:
		s.awaiting = &awaiting{intent, p, e.Seq, e.Cause}
	}
}

func trialFromIntent(p *journal.TrialIntent) Trial {
	t := Trial{Regime: p.Regime, Phase: p.Phase, Condition: p.Condition, Cores: slices.Clone(p.Cores), Workload: p.Workload, DurationS: p.DurationS, Profile: slices.Clone(p.Profile), Cycle: p.Cycle, Hunt: p.Hunt, Group: p.Group, Round: p.Round, Rerun: p.Rerun, Step: p.Step}
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
		s.setFailureIndex(a.seq)
		s.awaiting = nil
	}
	profile := slices.Clone(p.Profile)
	if len(profile) != len(s.cores) {
		if intent := s.intents[p.Trial]; intent != nil {
			profile = slices.Clone(intent.Profile)
		}
	}
	failure := pendingFailure{seq: e.Seq, failure: p, profile: profile}
	if p.KnownFailure != 0 {
		s.retry = nil
		if known := s.failureBySeq(p.KnownFailure); known != nil {
			failure = *known
			failure.seq, failure.failure = p.KnownFailure, p
		}
	}
	if p.KnownFailure != 0 {
		s.setFailureIndex(p.KnownFailure)
	} else if intent := s.intents[p.Trial]; intent != nil {
		failure.class = classOf(intent)
	} else if p.Trial == "" && (p.Condition == machine.Together || p.Condition == machine.Parked) {
		failure.class = trialClass{machine.R6, machine.Workloads(machine.R6)[0].ID, coresKey(s.ids()), s.durations.CheckingIdleS}
	}
	s.setFailureIndex(e.Seq)
	if s.multiR7(failure.class) {
		failure.loadBackoffs = slices.Clone(s.loads[loadOf(failure.class)])
	}
	s.pendingFailures = append(s.pendingFailures, failure)
	if s.multiR7(failure.class) {
		s.eachFailureEntry(failure.class, e.Seq, func(i int) { s.ledger[failure.class][i].named = p.Core })
	}
	if failure.class.regime == machine.R7 && len(s.classCores(failure.class)) > 1 {
		if c := s.locatedCulprit(failure); c != nil {
			c.pending = failure.seq
		}
		s.projectionDirty = true
		return
	}
	if s.zeroRerunFailure(p) {
		s.projectionDirty = true
		return
	}
	if p.Attribution == journal.Attributed && p.Core != nil {
		if c := s.core(*p.Core); c != nil {
			c.pending = failure.seq
		}
	}
	if p.Attribution == journal.Unattributed && (p.Condition == machine.Together || p.Condition == machine.Parked) {
		if p.Condition != machine.Parked || s.intents[p.Trial] == nil || p.Trial == "" {
			s.queue = append(s.queue, failure)
		}
	}
	if p.KnownFailure == 0 && p.Trial == "" && (p.Condition == machine.Together || p.Condition == machine.Parked) && p.Attribution == journal.Unattributed {
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
	case machine.Alone:
		f = attributeAlone(a.intent, a.end.Signal)
	case machine.Together, machine.Parked:
		f = s.attributeTogether(a)
	}
	if f == nil {
		return Action{}, false
	}
	return Action{Kind: Decide, Payload: f, Cause: append([]int{a.seq}, a.cause...)}, true
}

func (s *State) warningAction() Action {
	cause := []int{s.warningSeq}
	for _, seq := range s.warning.Passes {
		if _, carried := s.carriedSources[seq]; carried {
			cause = append(cause, seq)
		}
	}
	warning := *s.warning
	warning.Detail += s.carriedReason(cause)
	return Action{Kind: Decide, Payload: &warning, Cause: cause}
}

func unattributedFailureAtZero(seq int) Action {
	return Action{Kind: Decide, Payload: &journal.DeadEnd{Condition: journal.DeadEndFailureAtZero, Detail: "unattributed failure with every core at CO 0; the instability is not caused by Curve Optimizer"}, Cause: []int{seq}}
}

func (s *State) Drain() (Action, bool) {
	if a, ok := s.Attribution(); ok {
		return a, true
	}
	if s.warning != nil {
		return s.warningAction(), true
	}
	if len(s.queue) > 0 && allZero(s.queue[0].profile) {
		return unattributedFailureAtZero(s.queue[0].seq), true
	}
	if a, ok := s.pendingDecision(); ok && a.Kind == Decide {
		return a, true
	}
	if s.hunt != nil {
		if a, ok := s.huntNext(); ok && a.Kind == Decide {
			switch p := a.Payload.(type) {
			case *journal.HuntEnd, *journal.Combination:
				return a, true
			case *journal.TunerDecision:
				if p.Decision == journal.Backoff {
					return a, true
				}
			}
		}
	}
	return Action{}, false
}

func (s *State) Next() Action {
	a := s.next()
	if a.Kind == RunTrial && a.Trial.Regime == machine.R7 && len(a.Trial.Cores) > 1 && s.rankingSeq == 0 {
		return Action{Kind: ReadRanking}
	}
	return s.skipKnownFailure(a)
}

func (s *State) next() Action {
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
		return s.warningAction()
	}
	if a, ok := s.queuedReset(); ok {
		return a
	}
	for _, c := range s.cores {
		if c.fail != nil && *c.fail == 0 {
			return Action{Kind: Decide, Payload: &journal.DeadEnd{Condition: journal.DeadEndFailureAtZero, Core: new(c.id), Detail: fmt.Sprintf("core %02d has a failure point at CO 0; only reset can clear it", c.id)}, Cause: []int{c.zeroSeq}}
		}
	}
	if len(s.queue) > 0 && allZero(s.queue[0].profile) {
		return unattributedFailureAtZero(s.queue[0].seq)
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
		return Action{Kind: Decide, Payload: &journal.DeepeningRound{Round: s.round.start.Round, Event: journal.CycleEnd, Reason: "a failure needs a hunt"}, Cause: []int{s.queue[0].seq}}
	}
	if s.round != nil {
		if a, ok := s.roundMoves(); ok {
			return a
		}
	}
	if a, ok := s.phaseNext(); ok {
		return a
	}
	if !s.anySearch() && !slices.Equal(s.checking.profile, s.offsets()) {
		return s.profileNext()
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
	if a, ok := s.locateNext(); ok {
		return a
	}
	if a, ok := s.rerunNext(); ok {
		return a
	}
	if s.round != nil {
		return s.afterReruns(s.roundCheck())
	}
	if s.checking.open {
		a := s.cycleNext()
		_, startsStep := a.Payload.(*journal.CheckingStep)
		if _, chain := a.Payload.(*journal.CheckingChain); chain {
			startsStep = true
		}
		if a.Kind == RunTrial || startsStep {
			if end, ok := s.coveredEnd(); ok {
				a = end
			}
		}
		return s.afterReruns(a)
	}
	if s.deepeningDue() {
		if s.rankingSeq <= s.lastPlanSeq {
			return Action{Kind: ReadRanking}
		}
		return s.afterReruns(s.roundStart())
	}
	return s.afterReruns(Action{Kind: Decide, Payload: &journal.CheckingCycle{Cycle: s.checking.cycle + 1, Event: journal.CycleStart, Steps: slices.Clone(s.steps)}, Cause: []int{s.checking.lastSeq}})
}

func (s *State) anySearch() bool {
	return slices.ContainsFunc(s.cores, func(c *core) bool { return c.phase == journal.PhaseSearch })
}

func (s *State) profileNext() Action {
	var causes []int
	for _, c := range s.cores {
		if c.decisionSeq > s.checking.profileSeq {
			causes = append(causes, c.decisionSeq)
		}
	}
	slices.Sort(causes)
	return Action{Kind: Decide, Payload: &journal.ProfileChange{From: slices.Clone(s.checking.profile), To: s.offsets()}, Cause: causes}
}

func (s *State) phaseNext() (Action, bool) {
	if s.phasesSettled && s.phasesEpoch == s.limitEpoch {
		return Action{}, false
	}
	a, ok := s.computePhaseNext()
	s.phasesSettled, s.phasesEpoch = !ok, s.limitEpoch
	return a, ok
}

func (s *State) computePhaseNext() (Action, bool) {
	p := s.offsets()
	for _, c := range s.cores {
		if c.phase == journal.PhaseSearch {
			continue
		}
		reason, atLimit := s.atLimit(c, p)
		want := journal.PhaseHasRoom
		if atLimit {
			want = journal.PhaseAtLimit
		} else {
			reason = "one count deeper reaches no failure point or combination"
		}
		if c.phase != want {
			return Action{Kind: Decide, Payload: &journal.CorePhase{Core: c.id, From: c.phase, To: want, Offset: c.offset, Pass: c.pass, FailurePoint: c.fail, Reason: reason}, Cause: []int{c.decisionSeq}}, true
		}
	}
	return Action{}, false
}

func (s *State) Profile() []int  { return slices.Clone(s.checking.profile) }
func (s *State) ProfileSeq() int { return s.checking.profileSeq }
func (s *State) CleanCycles() int {
	n := 0
	for _, q := range s.passedFullCycles {
		if s.eligibleCleanCycle(q) {
			n++
		}
	}
	return n
}

func (s *State) eligibleCleanCycle(q passedFullCycle) bool {
	return q.allAtLimit && (q.seq > s.lastDeepenSeq || s.uncontradicted(q))
}

func (s *State) uncontradicted(q passedFullCycle) bool {
	if q.seq <= s.resetSeq || !AtLeastDeep(q.profile, s.checking.profile) {
		return false
	}
	return !slices.ContainsFunc(s.pendingFailures, func(f pendingFailure) bool {
		return s.failureAfter(f, s.resetSeq) && (len(f.profile) != len(q.profile) || AtLeastShallow(f.profile, q.profile))
	})
}

func (s *State) Project(st *journal.State) {
	st.Phase = string(journal.PhaseChecking)
	switch {
	case s.anySearch():
		st.Phase = string(journal.PhaseSearch)
	case s.hunt != nil:
		st.Phase = string(journal.PhaseHunt)
	case s.round != nil:
		st.Phase = string(journal.PhaseDeepening)
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
		x.FailurePoint = c.fail
		x.Queued = c.queued
		x.Combinations = nil
		for _, m := range s.combinations {
			for _, member := range m.Members {
				if member.Core == c.id {
					x.Combinations = append(x.Combinations, m.Combination)
				}
			}
		}
	}
	st.Combinations = slices.Clone(s.combinations)
	st.Hunt = s.projectHunt()
	st.Deepening = s.projectRound()
	st.Checking = s.projectChecking()
	s.projectionDirty = false
}
