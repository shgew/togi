package tuner

import (
	"fmt"
	"slices"

	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
)

type guard struct {
	profile    []int
	profileSeq int
	// dirty means an offset changed since the last profile.change; it starts true so guard begins with one.
	dirty    bool
	dirtySeq int

	rotation  int
	open      bool
	startSeq  int
	steps     []machine.Regime
	stepsDone int
	// passed holds the cores that passed the current per-core step.
	passed  []int
	// partsPassed holds the indexes into State.parts that passed the current R7 step.
	partsPassed []int
	// r7Index counts the R7 steps concluded this session; it picks the R7 workload.
	r7Index int
	lastSeq int
	endSeq  int

	cleanRotations int
	cleanS         int
	cleanByRegime  map[machine.Regime]int

	window   bool
	closeDue bool

	tctlMax *int
	tctlSeq int
}

// suspect is an unattributed resident failure whose suspect backoffs and window opening are still due.
type suspect struct {
	seq        int
	failure    *journal.Failure
	scope      string
	pending    []int
	backedOff  int
	openWindow bool
}

func (g *guard) changed(seq int, to []int) {
	g.profile, g.profileSeq, g.lastSeq = slices.Clone(to), seq, seq
	g.dirty = false
	g.cleanRotations, g.cleanS = 0, 0
	g.cleanByRegime = map[machine.Regime]int{}
	g.tctlMax, g.tctlSeq = nil, 0
}

func (g *guard) rotated(seq int, p *journal.GuardRotation) {
	g.lastSeq = seq
	switch p.Event {
	case journal.RotationStart:
		g.rotation, g.open, g.startSeq = p.Rotation, true, seq
		g.steps, g.stepsDone, g.passed, g.partsPassed = slices.Clone(p.Steps), 0, nil, nil
	case journal.RotationEnd:
		g.open = false
		if p.Clean {
			g.cleanRotations++
			g.endSeq = seq
			g.closeDue = g.window
		}
	}
}

func (s *State) foldWindow(p *journal.EscalationWindow) {
	switch p.State {
	case journal.WindowOpen:
		s.guard.window = true
		if sp := s.suspect; sp != nil {
			sp.openWindow = false
			if len(sp.pending) == 0 {
				s.suspect = nil
			}
		}
	case journal.WindowClose:
		s.guard.window, s.guard.closeDue = false, false
	}
}

func (s *State) foldResidentEnd(e journal.Event, p *journal.TrialEnd, intent *journal.TrialIntent) {
	g := &s.guard
	switch p.Outcome {
	case journal.OutcomePass:
		g.lastSeq = e.Seq
		g.cleanS += p.DurationS
		g.cleanByRegime[intent.Regime] += p.DurationS
		s.tierCause = e.Seq
		if p.TctlMaxC != nil && (g.tctlMax == nil || *p.TctlMaxC > *g.tctlMax) {
			g.tctlMax, g.tctlSeq = new(*p.TctlMaxC), e.Seq
		}
		if !g.open || intent.Rotation != g.rotation || g.stepsDone >= len(g.steps) || intent.Regime != g.steps[g.stepsDone] {
			return
		}
		if intent.Regime == machine.R7 {
			i := slices.IndexFunc(s.parts, func(part []int) bool { return slices.Equal(part, intent.Cores) })
			if i >= 0 && !slices.Contains(g.partsPassed, i) {
				g.partsPassed = append(g.partsPassed, i)
			}
			if len(g.partsPassed) == len(s.parts) {
				g.stepsDone++
				g.partsPassed = nil
				g.r7Index++
			}
			return
		}
		if intent.Core == nil {
			g.stepsDone++
			return
		}
		if !slices.Contains(g.passed, *intent.Core) {
			g.passed = append(g.passed, *intent.Core)
		}
		if len(g.passed) == len(s.cores) {
			g.stepsDone++
			g.passed = nil
		}
	case journal.OutcomeInconclusive:
		g.lastSeq = e.Seq
		t := residentTrial(intent)
		t.Retry = true
		s.retry = &t
	case journal.OutcomeFailure:
		if intent.Regime == machine.R7 {
			g.r7Index++
		}
		s.awaiting = &awaiting{intent: intent, end: p, seq: e.Seq, cause: e.Cause}
	}
}

func residentTrial(intent *journal.TrialIntent) Trial {
	t := Trial{Regime: intent.Regime, Phase: intent.Phase, Condition: machine.Resident, Rotation: intent.Rotation, Cores: slices.Clone(intent.Cores), Workload: intent.Workload}
	if intent.Core != nil {
		t.Core = *intent.Core
	}
	if intent.Offset != nil {
		t.Offset = *intent.Offset
	}
	return t
}

// attributeResident names the backend instance's core, else the one core that core-local MCEs among the trial end's
// causes name; any other evidence is unattributed.
func (s *State) attributeResident(a *awaiting) *journal.Failure {
	f := &journal.Failure{Signal: a.end.Signal, Attribution: journal.Unattributed, Trial: a.intent.Trial, Regime: a.intent.Regime, Condition: machine.Resident}
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
	if len(named) != 1 {
		return f
	}
	if c := s.core(named[0]); c != nil {
		f.Attribution, f.Core, f.Offset = journal.Attributed, new(c.id), new(c.offset)
	}
	return f
}

func (s *State) newSuspect(seq int, p *journal.Failure) *suspect {
	sp := &suspect{seq: seq, failure: p, openWindow: !s.guard.window}
	var target *core
	if intent := s.intents[p.Trial]; p.Trial != "" && intent != nil && intent.Core != nil && !p.Regime.AllCores() {
		target = s.core(*intent.Core)
	}
	switch {
	case s.guard.window:
		sp.scope = "escalation window open: every core backs off"
	case target != nil && target.offset < 0:
		sp.scope = "the only loaded core"
		sp.pending = []int{target.id}
		return sp
	case target != nil:
		sp.scope = fmt.Sprintf("the only loaded core %02d is at CO 0: every core backs off", target.id)
	default:
		sp.scope = fmt.Sprintf("%s involves every core", p.Regime)
	}
	for _, c := range s.cores {
		if c.offset < 0 {
			sp.pending = append(sp.pending, c.id)
		}
	}
	return sp
}

func (s *State) suspectBackedOff(core int) {
	sp := s.suspect
	if sp == nil {
		return
	}
	sp.pending = slices.DeleteFunc(sp.pending, func(c int) bool { return c == core })
	sp.backedOff++
	if len(sp.pending) == 0 && !sp.openWindow {
		s.suspect = nil
	}
}

func (s *State) suspectNext() (Action, bool) {
	sp := s.suspect
	if sp == nil {
		return Action{}, false
	}
	cause := []int{sp.seq}
	switch {
	case len(sp.pending) > 0:
		return Action{Kind: Decide, Payload: suspectBackoff(s.core(sp.pending[0]).snapshot(), sp), Cause: cause}, true
	case sp.backedOff == 0:
		detail := fmt.Sprintf("unattributed %s failure with every core at CO 0; the instability is not caused by Curve Optimizer", sp.failure.Signal)
		return Action{Kind: Decide, Payload: &journal.DeadEnd{Condition: journal.DeadEndFailureAtZero, Detail: detail}, Cause: cause}, true
	}
	return Action{Kind: Decide, Payload: &journal.EscalationWindow{State: journal.WindowOpen, Reason: "a further unattributed failure before a clean rotation backs off every core"}, Cause: cause}, true
}

func (s *State) guardNext() Action {
	g := &s.guard
	decide := func(p journal.Payload, cause ...int) Action { return Action{Kind: Decide, Payload: p, Cause: cause} }
	switch {
	case g.dirty:
		var to []int
		var cause []int
		for _, c := range s.byID() {
			to = append(to, c.offset)
			if c.decisionSeq > g.profileSeq {
				cause = append(cause, c.decisionSeq)
			}
		}
		slices.Sort(cause)
		return decide(&journal.ProfileChange{From: slices.Clone(g.profile), To: to}, cause...)
	case g.open && g.stepsDone == len(g.steps):
		return decide(&journal.GuardRotation{Rotation: g.rotation, Event: journal.RotationEnd, Clean: true}, g.startSeq, g.lastSeq)
	case g.closeDue:
		return decide(&journal.EscalationWindow{State: journal.WindowClose, Reason: fmt.Sprintf("rotation %d completed clean", g.rotation)}, g.endSeq)
	case !g.open:
		return decide(&journal.GuardRotation{Rotation: g.rotation + 1, Event: journal.RotationStart, Steps: slices.Clone(s.steps)}, g.lastSeq)
	}
	cause := []int{g.lastSeq}
	if s.retry != nil && s.retry.Condition == machine.Resident && s.retry.Rotation == g.rotation {
		return Action{Kind: RunTrial, Trial: *s.retry, Cause: cause}
	}
	t := Trial{Regime: g.steps[g.stepsDone], Phase: journal.PhaseGuard, Condition: machine.Resident, Rotation: g.rotation}
	switch t.Regime {
	case machine.R7:
		for i, part := range s.parts {
			if !slices.Contains(g.partsPassed, i) {
				t.Cores = slices.Clone(part)
				break
			}
		}
		t.Workload = machine.PickWorkload(machine.R7, g.r7Index).ID
		return Action{Kind: RunTrial, Trial: t, Cause: cause}
	case machine.R6:
		for _, c := range s.byID() {
			t.Cores = append(t.Cores, c.id)
		}
		return Action{Kind: RunTrial, Trial: t, Cause: cause}
	case machine.R1, machine.R2, machine.R3, machine.R4, machine.R5:
	}
	for _, c := range s.cores {
		if !slices.Contains(g.passed, c.id) {
			t.Core, t.Offset = c.id, c.offset
			break
		}
	}
	return Action{Kind: RunTrial, Trial: t, Cause: cause}
}

func (g *guard) project() *journal.GuardState {
	if g.profileSeq == 0 {
		return nil
	}
	regimes := make([]journal.RegimeClean, len(machine.Regimes))
	for i, r := range machine.Regimes {
		regimes[i] = journal.RegimeClean{Regime: r, CleanS: g.cleanByRegime[r], RateBoundPerH: rateBound(g.cleanByRegime[r])}
	}
	return &journal.GuardState{
		Rotation:         g.rotation,
		RotationOpen:     g.open,
		Steps:            slices.Clone(g.steps),
		StepsDone:        g.stepsDone,
		Profile:          slices.Clone(g.profile),
		ProfileSeq:       g.profileSeq,
		CleanRotations:   g.cleanRotations,
		CleanS:           g.cleanS,
		Regimes:          regimes,
		EscalationWindow: g.window,
		RateBoundPerH:    rateBound(g.cleanS),
		TctlMaxC:         g.tctlMax,
		TctlMaxSeq:       g.tctlSeq,
	}
}
