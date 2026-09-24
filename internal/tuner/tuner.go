// Package tuner is the pure per-core decision engine: a fold of journal events that proposes the next decision or trial.
package tuner

import (
	"fmt"
	"slices"

	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
)

type ActionKind int

const (
	Decide ActionKind = iota
	RunTrial
	EnterGuard
)

type Action struct {
	Kind    ActionKind
	Payload journal.Payload
	Trial   Trial
	Cause   []int
}

type Trial struct {
	Core   int
	Offset int
	Regime machine.Regime
	Phase  journal.Phase
	Retry  bool
}

type awaiting struct {
	intent *journal.TrialIntent
	signal machine.Signal
	seq    int
	cause  []int
}

type core struct {
	id         int
	phase      journal.Phase
	offset     int
	pass       *int
	fail       *int
	passed     []machine.Regime
	passedSeqs []int
	awaiting   *awaiting
	pending    int
	lastSeq    int
	zeroSeq    int
}

type State struct {
	cores   []*core
	cursor  int
	retry   *Trial
	intents map[string]*journal.TrialIntent
}

func New() *State {
	return &State{cursor: -1, intents: map[string]*journal.TrialIntent{}}
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
	case *journal.CorePhase:
		if c := s.core(p.Core); c != nil {
			c.phase, c.offset, c.pass, c.fail = p.To, p.Offset, p.Pass, p.FailedMark
			c.resetStep(e.Seq)
		}
	case *journal.TunerDecision:
		if c := s.core(p.Core); c != nil {
			c.offset, c.pass, c.fail = p.ToOffset, p.Pass, p.FailedMark
			c.resetStep(e.Seq)
		}
	case *journal.TrialIntent:
		s.intents[p.Trial] = p
		s.retry = nil
		if p.Core != nil {
			s.cursor = slices.IndexFunc(s.cores, func(c *core) bool { return c.id == *p.Core })
		}
	case *journal.TrialEnd:
		s.foldTrialEnd(e, p)
	case *journal.Failure:
		if p.Attribution != journal.Attributed || p.Trial == "" || p.Core == nil {
			return
		}
		if c := s.core(*p.Core); c != nil {
			if c.awaiting != nil && c.awaiting.intent.Trial == p.Trial {
				c.awaiting = nil
			}
			c.pending = e.Seq
		}
	case *journal.DeadEnd:
		if p.Condition != journal.DeadEndFailureAtZero || p.Core == nil {
			return
		}
		if c := s.core(*p.Core); c != nil {
			c.fail = new(0)
			c.zeroSeq = e.Seq
			c.pending = 0
		}
	}
}

func (c *core) resetStep(seq int) {
	c.passed, c.passedSeqs, c.pending = nil, nil, 0
	c.lastSeq = seq
}

func (s *State) foldTrialEnd(e journal.Event, p *journal.TrialEnd) {
	intent := s.intents[p.Trial]
	if intent == nil || intent.Core == nil || intent.Offset == nil {
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
			c.passed = append(c.passed, intent.Regime)
			c.passedSeqs = append(c.passedSeqs, e.Seq)
		}
	case journal.OutcomeInconclusive:
		s.retry = &Trial{Core: c.id, Offset: *intent.Offset, Regime: intent.Regime, Phase: intent.Phase, Retry: true}
	case journal.OutcomeFailure:
		c.awaiting = &awaiting{intent: intent, signal: p.Signal, seq: e.Seq, cause: e.Cause}
	}
}

func (s *State) Attribution() (Action, bool) {
	for _, c := range s.cores {
		if a := c.awaiting; a != nil {
			return Action{
				Kind:    Decide,
				Payload: attributeIsolated(a.intent, a.signal),
				Cause:   append([]int{a.seq}, a.cause...),
			}, true
		}
	}
	return Action{}, false
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
	for _, c := range s.cores {
		if c.active() && c.fail != nil && *c.fail == 0 {
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
	if s.cursor >= 0 {
		c := s.cores[s.cursor]
		switch c.phase {
		case journal.PhaseSearch:
			if c.hasPassed(machine.R1, machine.R2) {
				return Action{Kind: Decide, Payload: searchPass(c.snapshot()), Cause: slices.Clone(c.passedSeqs)}
			}
		case journal.PhaseConfirmation:
			if c.hasPassed(machine.ConfirmationRegimes...) {
				return Action{Kind: Decide, Payload: confirmed(c.snapshot()), Cause: slices.Clone(c.passedSeqs)}
			}
		case journal.PhaseConfirmed:
		}
	}
	if s.retry != nil {
		return s.runTrial(*s.retry)
	}
	if s.cursor >= 0 {
		c := s.cores[s.cursor]
		if c.phase == journal.PhaseSearch && slices.Equal(c.passed, []machine.Regime{machine.R1}) {
			return s.runTrial(Trial{Core: c.id, Offset: c.offset, Regime: machine.R2, Phase: c.phase})
		}
	}
	for i := range s.cores {
		c := s.cores[(s.cursor+1+i)%len(s.cores)]
		switch c.phase {
		case journal.PhaseSearch:
			return s.runTrial(Trial{Core: c.id, Offset: c.offset, Regime: machine.R1, Phase: c.phase})
		case journal.PhaseConfirmation:
			for _, r := range machine.ConfirmationRegimes {
				if !slices.Contains(c.passed, r) {
					return s.runTrial(Trial{Core: c.id, Offset: c.offset, Regime: r, Phase: c.phase})
				}
			}
		case journal.PhaseConfirmed:
		}
	}
	return Action{Kind: EnterGuard}
}

func (s *State) runTrial(t Trial) Action {
	return Action{Kind: RunTrial, Trial: t, Cause: []int{s.core(t.Core).lastSeq}}
}

func (c *core) active() bool {
	return c.phase == journal.PhaseSearch || c.phase == journal.PhaseConfirmation
}

func (c *core) hasPassed(regimes ...machine.Regime) bool {
	for _, r := range regimes {
		if !slices.Contains(c.passed, r) {
			return false
		}
	}
	return true
}

func (c *core) snapshot() coreState {
	return coreState{core: c.id, phase: c.phase, offset: c.offset, pass: c.pass, fail: c.fail}
}

func (s *State) Project(st *journal.State) {
	st.Phase = "per_core"
	if len(s.cores) > 0 && !slices.ContainsFunc(s.cores, func(c *core) bool { return c.phase != journal.PhaseConfirmed }) {
		st.Phase = "guard"
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
	}
}
