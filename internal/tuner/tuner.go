// Package tuner is the pure decision engine: a fold of journal events that proposes the next decision or trial.
package tuner

import (
	"cmp"
	"fmt"
	"slices"

	"code.marleb.org/shgew/shycler/internal/config"
	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
)

// Ruleset must be bumped for changes to steps, offset range, phases, regimes, confirmation, tiers or backoffs; this is breaking.
const Ruleset = 1

type ActionKind int

const (
	Decide ActionKind = iota
	RunTrial
)

type Action struct {
	Kind    ActionKind
	Payload journal.Payload
	Trial   Trial
	Cause   []int
}

// Trial is one trial to run. AllCores trials target every core and leave Core and Offset zero.
type Trial struct {
	Core      int
	Offset    int
	Regime    machine.Regime
	Phase     journal.Phase
	Condition machine.Condition
	AllCores  bool
	Rotation  int
	Retry     bool
	// Workload is empty when the run loop picks it by its per-regime index.
	Workload string
}

// slot is one trial a core must pass; an empty workload accepts any workload of the regime.
type slot struct {
	regime   machine.Regime
	workload string
}

// confirmationSet is every R1 and R2 workload in catalog order, then R3, R4 and R5.
var confirmationSet = func() []slot {
	var set []slot
	for _, r := range []machine.Regime{machine.R1, machine.R2} {
		for _, w := range machine.Workloads(r) {
			set = append(set, slot{r, w.ID})
		}
	}
	return append(set, slot{regime: machine.R3}, slot{regime: machine.R4}, slot{regime: machine.R5})
}()

func passedSlot(intent *journal.TrialIntent) slot {
	if intent.Phase == journal.PhaseConfirmation && (intent.Regime == machine.R1 || intent.Regime == machine.R2) {
		return slot{intent.Regime, intent.Workload}
	}
	return slot{regime: intent.Regime}
}

type awaiting struct {
	intent *journal.TrialIntent
	end    *journal.TrialEnd
	seq    int
	cause  []int
}

type core struct {
	id          int
	phase       journal.Phase
	offset      int
	pass        *int
	fail        *int
	unproven    int
	passed      []slot
	passedSeqs  []int
	pending     int
	lastSeq     int
	zeroSeq     int
	decisionSeq int
	baseline    int
	queued      string
	queueSeq    int
}

type State struct {
	cores    []*core
	cursor   int
	retry    *Trial
	intents  map[string]*journal.TrialIntent
	awaiting *awaiting
	mces     map[int]*journal.MCE
	steps    []machine.Regime
	guard    guard
	suspect  *suspect
	tier     journal.Tier
	tierSeq  int
	// tierCause is the latest decision, profile change, rotation event or resident pass: what a tier change cites.
	tierCause int
}

func New() *State {
	return &State{
		cursor:  -1,
		intents: map[string]*journal.TrialIntent{},
		mces:    map[int]*journal.MCE{},
		steps:   config.Default().Guard.Rotation,
		guard:   guard{dirty: true, cleanByRegime: map[machine.Regime]int{}},
		tier:    journal.TierNone,
	}
}

func Order(cores []machine.CoreInfo) []int {
	byCCD := map[int][]int{}
	for _, c := range cores {
		byCCD[c.CCD] = append(byCCD[c.CCD], c.Core)
	}
	var ccds []int
	for ccd, ids := range byCCD {
		slices.Sort(ids)
		ccds = append(ccds, ccd)
	}
	slices.Sort(ccds)
	order := make([]int, 0, len(cores))
	for i := 0; len(order) < len(cores); i++ {
		for _, ccd := range ccds {
			if ids := byCCD[ccd]; i < len(ids) {
				order = append(order, ids[i])
			}
		}
	}
	return order
}

func (s *State) core(id int) *core {
	for _, c := range s.cores {
		if c.id == id {
			return c
		}
	}
	return nil
}

func (s *State) Fold(e journal.Event) {
	switch p := e.Data.(type) {
	case *journal.SessionStart:
		s.cores = nil
		for _, id := range Order(p.Cores) {
			s.cores = append(s.cores, &core{id: id})
		}
	case *journal.ConfigLoaded:
		s.steps = slices.Clone(p.Config.Guard.Rotation)
	case *journal.SessionBaseline:
		for i, c := range s.byID() {
			if i < len(p.Offsets) {
				c.baseline = p.Offsets[i]
			}
		}
	case *journal.CommandRegain:
		s.queueRegain(e.Seq, p.Cores)
	case *journal.CommandReset:
		if p.Core == nil {
			return
		}
		if c := s.core(*p.Core); c != nil {
			c.queued, c.queueSeq = queuedReset, e.Seq
		}
	case *journal.CorePhase:
		if c := s.core(p.Core); c != nil {
			c.phase, c.offset, c.pass, c.fail, c.unproven = p.To, p.Offset, p.Pass, p.FailedMark, p.UnprovenDepth
			c.queued = ""
			s.decided(c, e.Seq)
		}
	case *journal.TunerDecision:
		if c := s.core(p.Core); c != nil {
			c.offset, c.pass, c.fail, c.unproven = p.ToOffset, p.Pass, p.FailedMark, p.UnprovenDepth
			if c.queued == queuedRegain && c.unproven == 0 {
				c.queued = ""
			}
			s.decided(c, e.Seq)
			if p.Decision == journal.SuspectBackoff {
				s.suspectBackedOff(c.id)
			}
		}
	case *journal.TrialIntent:
		s.intents[p.Trial] = p
		s.retry = nil
		if p.Condition == machine.Isolated && p.Core != nil {
			s.cursor = slices.IndexFunc(s.cores, func(c *core) bool { return c.id == *p.Core })
		}
	case *journal.TrialEnd:
		s.foldTrialEnd(e, p)
	case *journal.MCE:
		s.mces[e.Seq] = p
	case *journal.Failure:
		s.foldFailure(e, p)
	case *journal.DeadEnd:
		if p.Condition != journal.DeadEndFailureAtZero {
			return
		}
		if p.Core == nil {
			s.suspect = nil
			return
		}
		if c := s.core(*p.Core); c != nil {
			c.fail = new(0)
			c.zeroSeq = e.Seq
			c.pending = 0
		}
	case *journal.ProfileChange:
		s.guard.changed(e.Seq, p.To)
		s.tierCause = e.Seq
	case *journal.GuardRotation:
		s.guard.rotated(e.Seq, p)
		s.tierCause = e.Seq
	case *journal.EscalationWindow:
		s.foldWindow(p)
	case *journal.TierChange:
		s.tier, s.tierSeq = p.To, e.Seq
	}
}

func (s *State) decided(c *core, seq int) {
	c.passed, c.passedSeqs, c.pending = nil, nil, 0
	c.lastSeq = seq
	c.decisionSeq = seq
	s.guard.dirty, s.guard.dirtySeq = true, seq
	s.tierCause = seq
	if s.retry != nil && s.retry.Condition == machine.Isolated && s.retry.Core == c.id {
		s.retry = nil
	}
}

func (s *State) foldTrialEnd(e journal.Event, p *journal.TrialEnd) {
	intent := s.intents[p.Trial]
	if intent == nil {
		return
	}
	if intent.Condition == machine.Resident {
		s.foldResidentEnd(e, p, intent)
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
		if intent.Phase == c.phase && *intent.Offset == c.offset {
			c.passed = append(c.passed, passedSlot(intent))
			c.passedSeqs = append(c.passedSeqs, e.Seq)
		}
	case journal.OutcomeInconclusive:
		s.retry = &Trial{Core: c.id, Offset: *intent.Offset, Regime: intent.Regime, Phase: intent.Phase, Condition: machine.Isolated, Retry: true, Workload: intent.Workload}
	case journal.OutcomeFailure:
		s.awaiting = &awaiting{intent: intent, end: p, seq: e.Seq, cause: e.Cause}
	}
}

func (s *State) foldFailure(e journal.Event, p *journal.Failure) {
	if a := s.awaiting; a != nil && a.intent.Trial == p.Trial {
		s.awaiting = nil
	}
	switch {
	case p.Attribution == journal.Attributed && p.Trial != "" && p.Core != nil:
		if c := s.core(*p.Core); c != nil {
			c.pending = e.Seq
		}
	case p.Attribution == journal.Unattributed && p.Condition == machine.Resident:
		s.suspect = s.newSuspect(e.Seq, p)
	}
}

func (s *State) Attribution() (Action, bool) {
	a := s.awaiting
	if a == nil {
		return Action{}, false
	}
	var f *journal.Failure
	switch a.intent.Condition {
	case machine.Resident:
		f = s.attributeResident(a)
	case machine.Isolated:
		f = attributeIsolated(a.intent, a.end.Signal)
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
	if a, ok := s.queued(queuedReset); ok {
		return a
	}
	for _, c := range s.cores {
		if c.fail != nil && *c.fail == 0 {
			return Action{
				Kind:    Decide,
				Payload: &journal.DeadEnd{Condition: journal.DeadEndFailureAtZero, Core: new(c.id), Detail: fmt.Sprintf("core %02d has a failed mark at CO 0; only reset can clear it", c.id)},
				Cause:   []int{c.zeroSeq},
			}
		}
	}
	for _, c := range s.cores {
		if c.pending != 0 {
			return Action{Kind: Decide, Payload: failureRule(c.snapshot()), Cause: []int{c.pending}}
		}
	}
	if a, ok := s.suspectNext(); ok {
		return a
	}
	if a, ok := s.queued(queuedRegain); ok {
		return a
	}
	if g := &s.guard; g.open && g.dirty {
		return Action{Kind: Decide, Payload: &journal.GuardRotation{Rotation: g.rotation, Event: journal.RotationEnd, Reason: "the profile changed"}, Cause: []int{g.dirtySeq}}
	}
	if a, ok := s.tierNext(); ok {
		return a
	}
	if s.anyActive() {
		if a, ok := s.perCore(); ok {
			return a
		}
	}
	return s.guardNext()
}

func (s *State) perCore() (Action, bool) {
	if s.cursor >= 0 {
		c := s.cores[s.cursor]
		switch c.phase {
		case journal.PhaseSearch:
			if c.hasPassed(slot{regime: machine.R1}, slot{regime: machine.R2}) {
				return Action{Kind: Decide, Payload: searchPass(c.snapshot()), Cause: slices.Clone(c.passedSeqs)}, true
			}
		case journal.PhaseConfirmation:
			if c.hasPassed(confirmationSet...) {
				return Action{Kind: Decide, Payload: confirmed(c.snapshot()), Cause: slices.Clone(c.passedSeqs)}, true
			}
		case journal.PhaseRegain:
			if c.hasPassed(regainSet...) {
				return Action{Kind: Decide, Payload: regained(c.snapshot()), Cause: slices.Clone(c.passedSeqs)}, true
			}
		case journal.PhaseConfirmed, journal.PhaseGuard:
		}
	}
	if s.retry != nil && s.retry.Condition == machine.Isolated {
		return s.runTrial(*s.retry), true
	}
	if s.cursor >= 0 {
		c := s.cores[s.cursor]
		if c.phase == journal.PhaseSearch && slices.Equal(c.passed, []slot{{regime: machine.R1}}) {
			return s.runTrial(c.isolated(machine.R2)), true
		}
	}
	for i := range s.cores {
		c := s.cores[(s.cursor+1+i)%len(s.cores)]
		switch c.phase {
		case journal.PhaseSearch:
			return s.runTrial(c.isolated(machine.R1)), true
		case journal.PhaseConfirmation:
			for _, sl := range confirmationSet {
				if !slices.Contains(c.passed, sl) {
					t := c.isolated(sl.regime)
					t.Workload = sl.workload
					return s.runTrial(t), true
				}
			}
		case journal.PhaseRegain:
			for _, sl := range regainSet {
				if !slices.Contains(c.passed, sl) {
					return s.runTrial(c.isolated(sl.regime)), true
				}
			}
		case journal.PhaseConfirmed, journal.PhaseGuard:
		}
	}
	return Action{}, false
}

func (c *core) isolated(r machine.Regime) Trial {
	return Trial{Core: c.id, Offset: c.offset, Regime: r, Phase: c.phase, Condition: machine.Isolated}
}

func (s *State) runTrial(t Trial) Action {
	return Action{Kind: RunTrial, Trial: t, Cause: []int{s.core(t.Core).lastSeq}}
}

func (s *State) anyActive() bool {
	return slices.ContainsFunc(s.cores, (*core).active)
}

func (c *core) active() bool {
	return c.phase == journal.PhaseSearch || c.phase == journal.PhaseConfirmation || c.phase == journal.PhaseRegain
}

func (c *core) hasPassed(slots ...slot) bool {
	for _, sl := range slots {
		if !slices.Contains(c.passed, sl) {
			return false
		}
	}
	return true
}

var regainSet = []slot{{regime: machine.R1}, {regime: machine.R2}, {regime: machine.R3}, {regime: machine.R4}, {regime: machine.R5}}

func (c *core) snapshot() coreState {
	return coreState{core: c.id, phase: c.phase, offset: c.offset, pass: c.pass, fail: c.fail, unproven: c.unproven}
}

func (s *State) byID() []*core {
	return slices.SortedFunc(slices.Values(s.cores), func(a, b *core) int { return cmp.Compare(a.id, b.id) })
}

// Profile is the profile of the last profile.change, in core-id order.
func (s *State) Profile() []int { return slices.Clone(s.guard.profile) }

func (s *State) ProfileSeq() int { return s.guard.profileSeq }

// CleanRotations counts the clean rotations since the last profile.change.
func (s *State) CleanRotations() int { return s.guard.cleanRotations }

func (s *State) Project(st *journal.State) {
	st.Phase = "guard"
	switch {
	case len(s.cores) == 0 || slices.ContainsFunc(s.cores, func(c *core) bool { return c.phase == journal.PhaseSearch || c.phase == journal.PhaseConfirmation }):
		st.Phase = "per_core"
	case s.anyActive():
		st.Phase = "regain"
	}
	for i := range st.Cores {
		c := s.core(st.Cores[i].Core)
		if c == nil {
			continue
		}
		st.Cores[i].Offset = c.offset
		st.Cores[i].Phase = c.phase
		st.Cores[i].Pass = c.pass
		st.Cores[i].FailedMark = c.fail
		st.Cores[i].UnprovenDepth = c.unproven
		st.Cores[i].Queued = c.queued
	}
	st.Guard = s.guard.project()
	st.Tier, st.TierSeq = s.tier, s.tierSeq
}
