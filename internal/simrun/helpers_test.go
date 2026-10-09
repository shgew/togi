package simrun

import (
	"context"
	"encoding/json"
	"io/fs"
	"testing"

	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
)

func huntConfig(cores int) sim.Config {
	model := sim.DefaultModel()
	model.PastLimitRate = 1
	model.Signals = map[machine.Signal]float64{machine.Crash: 1}
	model.CrashMCE = 0
	limits := make([]sim.Limits, cores)
	for i := range limits {
		limits[i].Alone = [5]int{-10, -10, -10, -10, -10}
		limits[i].Together = [7]int{-10, -10, -10, -10, -10, -10, -50}
	}
	return sim.Config{Seed: 1, Cores: cores, Limits: limits, Model: &model}
}

func runHunt(t *testing.T, cfg sim.Config, setup func(*sim.Machine), tweak func(*Input)) (session.Stop, []journal.Event, string) {
	t.Helper()
	m, err := sim.New(cfg)
	if err != nil {
		t.Fatal(err)
	}
	if setup != nil {
		setup(m)
	}
	dir := t.TempDir()
	in := Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Cycles: 1, InMemoryJournal: true}
	if tweak != nil {
		tweak(&in)
	}
	stop, err := Simulate(context.Background(), in)
	if err != nil {
		t.Fatal(err)
	}
	events, torn, err := journal.Read(dir)
	if err != nil || torn != nil {
		t.Fatalf("read events: %v, torn %q", err, torn)
	}
	return stop, events, dir
}

func stopAfterAlonePasses(in *Input, cores int) {
	trials := map[string]int{}
	passed := map[int]bool{}
	in.Until = func(e journal.Event) bool {
		switch p := e.Data.(type) {
		case *journal.TrialIntent:
			if p.Condition == machine.Alone && p.Core != nil {
				trials[p.Trial] = *p.Core
			}
		case *journal.TrialEnd:
			if p.Outcome == journal.OutcomePass {
				if core, ok := trials[p.Trial]; ok {
					passed[core] = true
				}
			}
		}
		return len(passed) == cores
	}
}

func findPayload[P journal.Payload](events []journal.Event, accept func(P) bool) (P, bool) {
	var zero P
	for _, e := range events {
		p, ok := e.Data.(P)
		if ok && (accept == nil || accept(p)) {
			return p, true
		}
	}
	return zero, false
}

func quickMatrixConfig() config.Config {
	c := config.Default()
	c.CandidateSoloLimits = map[int]int{0: -10, 1: -10, 2: -10, 3: -10}
	c.Evidence.Miss = .5
	c.Durations.SearchTrialS = 1
	c.Durations.ShortTrialS = 1
	c.Durations.CheckingTrialS = 1
	c.Durations.CheckingIdleS = 1
	c.Durations.CheckingAllCoreS = 1
	return c
}

type resetAtEvent struct {
	session.Journal
	machine *sim.Machine
	kind    journal.Kind
	reset   machine.ResetKind
	fired   *bool
}

func (j *resetAtEvent) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := j.Journal.Append(p, cause...)
	if err == nil && !*j.fired && e.Kind == j.kind {
		*j.fired = true
		j.machine.NextReset(j.reset)
		j.machine.Crash()
		return e, machine.ErrCrashed
	}
	return e, err
}

func afterInitialPhases(n int) func(journal.Event) bool {
	phases := 0
	return func(e journal.Event) bool {
		if p, ok := e.Data.(*journal.CorePhase); ok && p.From == "" {
			phases++
		}
		return phases == n
	}
}

// memState stands in for state.json across the boots of one run while the journal is still reopened from disk at
// every boot. Like internal/session's tests it encodes the last written state when read, because a boot can end
// before the runner folds anything further into the state it last wrote.
type memState struct {
	data    []byte
	pending *journal.State
}

type memStateJournal struct {
	session.Journal
	state *memState
}

func (j *memStateJournal) WriteState(s journal.State) error {
	j.state.pending = &s
	return nil
}

func (j *memStateJournal) ReadState() (journal.State, error) {
	var s journal.State
	if p := j.state.pending; p != nil {
		data, err := json.Marshal(*p)
		if err != nil {
			return s, err
		}
		j.state.data, j.state.pending = data, nil
	}
	if j.state.data == nil {
		return s, fs.ErrNotExist
	}
	err := json.Unmarshal(j.state.data, &s)
	return s, err
}
