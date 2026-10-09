package tuner

import (
	"fmt"
	"slices"
	"testing"

	gocmp "github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type coreStart struct {
	phase      journal.Phase
	offset     int
	pass, fail *int
	check      bool
}

type soloKey struct {
	core   int
	regime machine.Regime
}

// harness folds the events it records into a reducer and keeps the workload rotation the runner derives from the
// same events, so completing a trial never reads the reducer's private state.
type harness struct {
	t         *testing.T
	s         *State
	events    []journal.Event
	trials    int
	ids       []int
	intents   map[string]*journal.TrialIntent
	allIndex  map[machine.Regime]int
	soloIndex map[soloKey]int
}

// topology lays n cores over two CCDs; coreStarts index into it.
func topology(n int) []machine.CoreInfo {
	infos := make([]machine.CoreInfo, n)
	half := max(n/2, 1)
	for i := range infos {
		infos[i] = machine.CoreInfo{Core: i, CCD: i / half, CPUs: []int{i, i + n}}
	}
	return infos
}

// onOneCCD returns the topology with every core on CCD 0.
func onOneCCD(infos []machine.CoreInfo) []machine.CoreInfo {
	out := slices.Clone(infos)
	for i := range out {
		out[i].CCD = 0
	}
	return out
}

func newHarness(t *testing.T, starts ...coreStart) *harness {
	t.Helper()
	return newHarnessOn(t, topology(len(starts)), config.Default(), starts...)
}

// bareHarness records no events yet; tests that need a SessionStart of their own add it.
func bareHarness(t *testing.T) *harness {
	t.Helper()
	return &harness{t: t, s: New(), intents: map[string]*journal.TrialIntent{}, allIndex: map[machine.Regime]int{}, soloIndex: map[soloKey]int{}}
}

// newHarnessOn starts a session on the given topology and configuration; starts[i] sets the core at infos[i].
func newHarnessOn(t *testing.T, infos []machine.CoreInfo, cfg config.Config, starts ...coreStart) *harness {
	t.Helper()
	h := bareHarness(t)
	begin := h.add(&journal.SessionStart{Schema: journal.Schema, Session: "s", Cores: infos})
	h.add(&journal.ConfigLoaded{Path: config.DefaultPath, Config: snapshotConfig(cfg)})
	for i, c := range starts {
		p := &journal.CorePhase{Core: infos[i].Core, To: c.phase, Offset: c.offset, Pass: c.pass, FailurePoint: c.fail, CheckSoloLimit: c.check, Reason: "test"}
		if c.check {
			p.Workloads = []string{machine.Workloads(machine.R1)[0].ID, machine.Workloads(machine.R2)[0].ID}
		}
		h.add(p, begin.Seq)
	}
	return h
}

func snapshotConfig(c config.Config) journal.ConfigSnapshot {
	return journal.ConfigSnapshot{
		StartOffsets:        c.StartOffsets,
		CandidateSoloLimits: c.CandidateSoloLimits,
		Durations:           journal.ConfigDurations(c.Durations),
		Evidence:            journal.ConfigEvidence(c.Evidence),
		Checking:            journal.ConfigChecking(c.Checking),
		DeadEnds:            journal.ConfigDeadEnds(c.DeadEnds),
		Backends:            journal.ConfigBackends(c.Backends),
		BackendUser:         c.BackendUser,
	}
}

func searchAt(offsets ...int) []coreStart {
	out := make([]coreStart, len(offsets))
	for i, x := range offsets {
		out[i] = coreStart{phase: journal.PhaseSearch, offset: x}
	}
	return out
}

func (h *harness) add(p journal.Payload, cause ...int) journal.Event {
	e := journal.Event{Seq: len(h.events) + 1, Kind: p.Kind(), Boot: "b", Msg: p.Message(), Data: p, Cause: cause}
	h.events = append(h.events, e)
	h.s.Fold(e)
	switch p := p.(type) {
	case *journal.SessionStart:
		h.ids = h.ids[:0]
		for _, info := range p.Cores {
			h.ids = append(h.ids, info.Core)
		}
		slices.Sort(h.ids)
	case *journal.TrialIntent:
		h.intents[p.Trial] = p
	case *journal.TrialEnd:
		intent := h.intents[p.Trial]
		if intent == nil || (p.Outcome != journal.OutcomePass && p.Outcome != journal.OutcomeFailure) {
			break
		}
		if intent.Core == nil {
			h.allIndex[intent.Regime]++
		} else {
			h.soloIndex[soloKey{*intent.Core, intent.Regime}]++
		}
	}
	return e
}

func (h *harness) start(a Action) journal.Event {
	h.t.Helper()
	if a.Kind != RunTrial {
		h.t.Fatalf("action %+v, want trial", a)
	}
	h.trials++
	tr := a.Trial
	index := h.allIndex[tr.Regime]
	if len(tr.Cores) == 0 {
		index = h.soloIndex[soloKey{tr.Core, tr.Regime}]
	}
	profile := tr.Profile
	if profile == nil {
		profile = make([]int, len(h.ids))
		if tr.Condition == machine.Alone {
			profile[slices.Index(h.ids, tr.Core)] = tr.Offset
		} else {
			copy(profile, h.s.Profile())
		}
	}
	p, _, err := tr.Complete(index, profile)
	if err != nil {
		h.t.Fatal(err)
	}
	p.Trial = fmt.Sprintf("%04d", h.trials)
	return h.add(p, a.Cause...)
}

func (h *harness) trial(a Action, end journal.TrialEnd) (journal.Event, journal.Event) {
	h.t.Helper()
	intent := h.start(a)
	end.Trial = intent.Data.(*journal.TrialIntent).Trial
	return intent, h.add(&end, intent.Seq)
}

func (h *harness) decide(a Action) journal.Event {
	h.t.Helper()
	if a.Kind != Decide {
		h.t.Fatalf("action %+v, want decision", a)
	}
	return h.add(a.Payload, a.Cause...)
}

func (h *harness) next() Action {
	h.t.Helper()
	for range 2000 {
		a := h.s.Next()
		if a.Kind == ReadRanking {
			h.add(&journal.HostRanking{Ranking: h.s.ids()})
			continue
		}
		return a
	}
	h.t.Fatal("stuck reading ranking")
	return Action{}
}

// replayState folds recorded events into a fresh reducer, as a resumed session does.
func replayState(events []journal.Event) *State {
	s := New()
	journal.Replay(events, s)
	return s
}

func projected(h *harness) journal.State {
	var st journal.State
	journal.Replay(h.events, &st)
	h.s.Project(&st)
	return st
}

func assertProjectionReplay(h *harness) {
	h.t.Helper()
	replayed := replayState(h.events)
	var st journal.State
	journal.Replay(h.events, &st)
	replayed.Project(&st)
	if diff := gocmp.Diff(projected(h), st, cmpopts.IgnoreUnexported(journal.State{})); diff != "" {
		h.t.Fatalf("projection replay (-live +replayed):\n%s", diff)
	}
}

var (
	passed = journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: 120}
	failed = journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, DurationS: 41}
	unsure = journal.TrialEnd{Outcome: journal.OutcomeInconclusive, Reason: "setup failed"}
)

func TestTrialCompletionSelectsReducerWorkload(t *testing.T) {
	for _, regime := range []machine.Regime{machine.R1, machine.R7} {
		t.Run(string(regime), func(t *testing.T) {
			h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -10})
			tr := Trial{Core: 0, Offset: -10, Regime: regime, Condition: machine.Together, DurationS: 120}
			if regime == machine.R7 {
				tr.Cores = []int{0}
			}
			for _, step := range []struct {
				end      journal.TrialEnd
				workload string
				want     int
			}{
				{passed, "", 0},
				{failed, "", 1},
				{unsure, "", 2},
				{passed, machine.Workloads(regime)[1].ID, 1},
				{passed, "", 0},
			} {
				tr.Workload = step.workload
				intent, _ := h.trial(Action{Kind: RunTrial, Trial: tr}, step.end)
				if diff := gocmp.Diff(machine.Workloads(regime)[step.want].ID, intent.Data.(*journal.TrialIntent).Workload); diff != "" {
					t.Fatalf("completed workload (-want +got):\n%s", diff)
				}
			}
		})
	}
}
