package sim

import (
	"fmt"
	"slices"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type ReplayFact struct {
	Context   machine.BIOSContext
	Class     journal.TrialClass
	Profile   []int
	Outcome   journal.Outcome
	Signal    machine.Signal
	DurationS int
}

type replayClass struct {
	regime   machine.Regime
	workload string
	duration time.Duration
}

type Replay struct {
	context machine.BIOSContext
	facts   map[replayClass][]ReplayFact
}

func NewReplay(context machine.BIOSContext, records []ReplayFact) (*Replay, error) {
	if context == (machine.BIOSContext{}) {
		return nil, fmt.Errorf("create replay oracle: BIOS context is required")
	}
	r := &Replay{context: context, facts: make(map[replayClass][]ReplayFact)}
	for i, f := range records {
		if f.Context != context || (f.Outcome != journal.OutcomePass && f.Outcome != journal.OutcomeFailure) {
			continue
		}
		if len(f.Class.Cores) == 0 || f.Class.DurationS <= 0 || f.DurationS < 0 || f.DurationS > f.Class.DurationS {
			return nil, fmt.Errorf("create replay oracle: fact %d has invalid cores or duration", i)
		}
		if f.Outcome == journal.OutcomeFailure && !slices.Contains(signalOrder, f.Signal) && f.Signal != machine.UncorrectedMCE {
			return nil, fmt.Errorf("create replay oracle: fact %d has unsupported failure signal %q", i, f.Signal)
		}
		f.Class.Cores = slices.Clone(f.Class.Cores)
		slices.Sort(f.Class.Cores)
		for j, core := range f.Class.Cores {
			if core < 0 || core >= len(f.Profile) || (j > 0 && f.Class.Cores[j-1] == core) {
				return nil, fmt.Errorf("create replay oracle: fact %d has invalid loaded core %d", i, core)
			}
		}
		f.Profile = slices.Clone(f.Profile)
		key := replayClass{f.Class.Regime, f.Class.Workload, time.Duration(f.Class.DurationS) * time.Second}
		r.facts[key] = append(r.facts[key], f)
	}
	return r, nil
}

func replayMatches(f ReplayFact, profile []int, spec machine.TrialSpec) bool {
	if !slices.Equal(f.Profile, profile) || len(f.Class.Cores) != len(spec.Cores) {
		return false
	}
	for _, core := range f.Class.Cores {
		if !slices.Contains(spec.Cores, core) {
			return false
		}
	}
	return true
}

func (m *Machine) replayFacts(spec machine.TrialSpec) []ReplayFact {
	r := m.cfg.Replay
	if r == nil || r.context != m.bios {
		return nil
	}
	return r.facts[replayClass{spec.Regime, spec.Workload.ID, spec.Duration}]
}

func (m *Machine) HasRealAnswer(profile []int, spec machine.TrialSpec) bool {
	for _, f := range m.replayFacts(spec) {
		if replayMatches(f, profile, spec) {
			return true
		}
	}
	return false
}

func (m *Machine) replayDraw(spec machine.TrialSpec) (ReplayFact, bool) {
	facts := m.replayFacts(spec)
	n := 0
	for _, f := range facts {
		if replayMatches(f, m.regs, spec) {
			n++
		}
	}
	if n == 0 {
		return ReplayFact{}, false
	}
	pick := m.rng("replay", spec.ID, spec.Index, spec.Regime, spec.Workload.ID, spec.Duration).IntN(n)
	for _, f := range facts {
		if replayMatches(f, m.regs, spec) {
			if pick == 0 {
				return f, true
			}
			pick--
		}
	}
	panic("replay draw exhausted matching facts")
}
