package tuner

import (
	"fmt"
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

type harness struct {
	t        *testing.T
	s        *State
	events   []journal.Event
	trials   int
	allIndex map[machine.Regime]int
}

func newHarness(t *testing.T, starts ...coreStart) *harness {
	t.Helper()
	h := &harness{t: t, s: New()}
	infos := make([]machine.CoreInfo, len(starts))
	half := max(len(starts)/2, 1)
	for i := range infos {
		infos[i] = machine.CoreInfo{Core: i, CCD: i / half, CPUs: []int{i, i + len(starts)}}
	}
	begin := h.add(&journal.SessionStart{Schema: journal.Schema, Session: "s", Cores: infos})
	h.add(&journal.ConfigLoaded{Path: config.DefaultPath, Config: snapshotConfig(config.Default())})
	for i, c := range starts {
		p := &journal.CorePhase{Core: i, To: c.phase, Offset: c.offset, Pass: c.pass, FailurePoint: c.fail, CheckSoloLimit: c.check, Reason: "test"}
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
	if end, ok := p.(*journal.TrialEnd); ok && (end.Outcome == journal.OutcomePass || end.Outcome == journal.OutcomeFailure) {
		if intent := h.s.intents[end.Trial]; intent != nil && intent.Core == nil {
			if h.allIndex == nil {
				h.allIndex = map[machine.Regime]int{}
			}
			h.allIndex[intent.Regime]++
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
		index = h.s.core(tr.Core).workloadIndex[tr.Regime]
	}
	profile := tr.Profile
	if profile == nil {
		profile = make([]int, len(h.s.cores))
		if tr.Condition == machine.Alone {
			profile[h.s.index(tr.Core)] = tr.Offset
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
