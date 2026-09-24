package tuner

import (
	"fmt"
	"testing"

	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
)

type coreStart struct {
	phase      journal.Phase
	offset     int
	pass, fail *int
	unproven   int
}

type harness struct {
	t      *testing.T
	s      *State
	events []journal.Event
	trials int
}

func newHarness(t *testing.T, cores ...coreStart) *harness {
	t.Helper()
	h := &harness{t: t, s: New()}
	infos := make([]machine.CoreInfo, len(cores))
	half := max(len(cores)/2, 1)
	for i := range cores {
		infos[i] = machine.CoreInfo{Core: i, CCD: i / half, CPUs: []int{i, i + len(cores)}}
	}
	start := h.add(&journal.SessionStart{Schema: journal.Schema, Session: "s", Cores: infos})
	for i, c := range cores {
		h.add(&journal.CorePhase{Core: i, To: c.phase, Offset: c.offset, Pass: c.pass, FailedMark: c.fail, UnprovenDepth: c.unproven, Reason: "test"}, start.Seq)
	}
	return h
}

func searchAt(offsets ...int) []coreStart {
	out := make([]coreStart, len(offsets))
	for i, o := range offsets {
		out[i] = coreStart{phase: journal.PhaseSearch, offset: o}
	}
	return out
}

func (h *harness) add(p journal.Payload, cause ...int) journal.Event {
	e := journal.Event{Seq: len(h.events) + 1, Kind: p.Kind(), Boot: "b", Msg: p.Message(), Data: p, Cause: cause}
	h.events = append(h.events, e)
	h.s.Fold(e)
	return e
}

func (h *harness) trial(a Action, end journal.TrialEnd) (intent, ended journal.Event) {
	h.t.Helper()
	intent = h.start(a)
	end.Trial = intent.Data.(*journal.TrialIntent).Trial
	return intent, h.add(&end, intent.Seq)
}

func (h *harness) start(a Action) journal.Event {
	h.t.Helper()
	if a.Kind != RunTrial {
		h.t.Fatalf("action %+v, want RunTrial", a)
	}
	h.trials++
	p := &journal.TrialIntent{
		Trial: fmt.Sprintf("%04d", h.trials), Regime: a.Trial.Regime, Workload: "w", DurationS: 90,
		Condition: a.Trial.Condition, Phase: a.Trial.Phase, Retry: a.Trial.Retry, Rotation: a.Trial.Rotation,
	}
	if a.Trial.AllCores {
		for _, c := range h.s.byID() {
			p.Cores = append(p.Cores, c.id)
		}
	} else {
		p.Core, p.Offset = new(a.Trial.Core), new(a.Trial.Offset)
	}
	return h.add(p, a.Cause...)
}

func (h *harness) decide(a Action) journal.Event {
	h.t.Helper()
	if a.Kind != Decide {
		h.t.Fatalf("action %+v, want Decide", a)
	}
	return h.add(a.Payload, a.Cause...)
}

var (
	passed = journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: 90}
	failed = journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, DurationS: 41}
	unsure = journal.TrialEnd{Outcome: journal.OutcomeInconclusive, Reason: "setup failed"}
)
