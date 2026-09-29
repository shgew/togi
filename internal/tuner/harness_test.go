package tuner

import (
	"cmp"
	"fmt"
	"testing"

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
	t      *testing.T
	s      *State
	events []journal.Event
	trials int
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
	h.add(&journal.ConfigLoaded{Path: config.DefaultPath, Config: config.Default()})
	for i, c := range starts {
		p := &journal.CorePhase{Core: i, To: c.phase, Offset: c.offset, Pass: c.pass, FailedMark: c.fail, CheckEdge: c.check, Reason: "test"}
		if c.check {
			p.Workloads = []string{machine.Workloads(machine.R1)[0].ID, machine.Workloads(machine.R2)[0].ID}
		}
		h.add(p, begin.Seq)
	}
	return h
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
	return e
}

func (h *harness) start(a Action) journal.Event {
	h.t.Helper()
	if a.Kind != RunTrial {
		h.t.Fatalf("action %+v, want trial", a)
	}
	h.trials++
	tr := a.Trial
	p := &journal.TrialIntent{Trial: fmt.Sprintf("%04d", h.trials), Regime: tr.Regime, Workload: cmp.Or(tr.Workload, machine.Workloads(tr.Regime)[0].ID), DurationS: tr.DurationS, Condition: tr.Condition, Phase: tr.Phase, Retry: tr.Retry, Rotation: tr.Rotation, Hunt: tr.Hunt, Mask: tr.Mask, Round: tr.Round, Rerun: tr.Rerun, Cores: tr.Cores, Profile: tr.Profile}
	if len(tr.Cores) == 0 {
		p.Core = new(tr.Core)
		p.Offset = new(tr.Offset)
	}
	if p.Profile == nil {
		p.Profile = make([]int, len(h.s.cores))
		if tr.Condition == machine.Isolated {
			p.Profile[h.s.index(tr.Core)] = tr.Offset
		} else {
			copy(p.Profile, h.s.Profile())
		}
	}
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

func projected(h *harness) journal.State {
	var st journal.State
	journal.Replay(h.events, &st)
	h.s.Project(&st)
	return st
}

var (
	passed = journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: 120}
	failed = journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, DurationS: 41}
	unsure = journal.TrialEnd{Outcome: journal.OutcomeInconclusive, Reason: "setup failed"}
)
