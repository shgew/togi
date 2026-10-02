package tuner

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestSearchAndEdgeCheck(t *testing.T) {
	h := newHarness(t, searchAt(-49)...)
	first := h.next()
	if first.Kind != RunTrial || first.Trial.Regime != machine.R1 || first.Trial.DurationS != h.s.durations.SearchTrialS {
		t.Fatalf("first search trial %+v", first)
	}
	h.trial(first, passed)
	second := h.next()
	if second.Trial.Regime != machine.R2 {
		t.Fatalf("R2 not next: %+v", second)
	}
	h.trial(second, passed)
	step := h.next()
	d, ok := step.Payload.(*journal.TunerDecision)
	if !ok || d.Decision != journal.StepDeeper || d.ToOffset != -50 {
		t.Fatalf("search step %+v", step)
	}
	h.decide(step)
	for _, r := range []machine.Regime{machine.R1, machine.R2} {
		a := h.next()
		if a.Trial.Regime != r {
			t.Fatalf("search step %+v", a)
		}
		h.trial(a, passed)
	}
	edge := h.next()
	d, ok = edge.Payload.(*journal.TunerDecision)
	if !ok || d.Decision != journal.CheckEdge || d.ToOffset != -50 || len(d.Workloads) != 2 {
		t.Fatalf("edge %+v", edge)
	}
	h.decide(edge)
	for _, r := range []machine.Regime{machine.R1, machine.R2} {
		for range h.s.n {
			a := h.next()
			if a.Kind != RunTrial || a.Trial.Regime != r || a.Trial.Workload == "" {
				t.Fatalf("check %+v", a)
			}
			h.trial(a, passed)
		}
	}
	a := h.next()
	phase, ok := a.Payload.(*journal.CorePhase)
	if !ok || phase.To != journal.PhaseDone || phase.Pass == nil || *phase.Pass != -50 {
		t.Fatalf("edge completion %+v", a)
	}
	h.decide(a)
	profile := h.next()
	if p, ok := profile.Payload.(*journal.ProfileChange); !ok || cmp.Diff([]int{-50}, p.To) != "" {
		t.Fatalf("profile %+v", profile)
	}
}

func TestSearchFailureAndRetry(t *testing.T) {
	h := newHarness(t, searchAt(-10)...)
	a := h.next()
	h.trial(a, unsure)
	retry := h.next()
	if !retry.Trial.Retry || retry.Trial.Regime != machine.R1 || retry.Trial.DurationS != a.Trial.DurationS {
		t.Fatalf("retry %+v", retry)
	}
	h.trial(retry, failed)
	f := h.next()
	if p, ok := f.Payload.(*journal.Failure); !ok || p.Attribution != journal.Attributed || *p.Core != 0 {
		t.Fatalf("failure %+v", f)
	}
	h.decide(f)
	back := h.next()
	if p, ok := back.Payload.(*journal.TunerDecision); !ok || p.Decision != journal.Backoff || p.ToOffset != -5 || *p.FailedMark != -10 {
		t.Fatalf("backoff %+v", back)
	}
}

func TestDecisionSupersedesIsolatedRetry(t *testing.T) {
	h := newHarness(t, searchAt(-10)...)
	h.trial(h.next(), unsure)
	h.add(&journal.CorePhase{Core: 0, To: journal.PhaseSearch, Offset: -5})
	a := h.next()
	if a.Kind != RunTrial || a.Trial.Retry || a.Trial.Offset != -5 {
		t.Fatalf("old retry survived phase decision: %+v", a)
	}
}

func TestSearchSkipsResidentCores(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseDone, offset: -50}, coreStart{phase: journal.PhaseSearch, offset: -10})
	a := h.next()
	if a.Kind != RunTrial || a.Trial.Core != 1 || a.Trial.Regime != machine.R1 || a.Trial.Condition != machine.Isolated {
		t.Fatalf("search scheduled resident core: %+v", a)
	}
}

func TestSeededCandidateEdgeFreezesWorkloads(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseSearch, offset: -20})
	w1, w2 := machine.Workloads(machine.R1)[1].ID, machine.Workloads(machine.R2)[2].ID
	h.add(&journal.CorePhase{Core: 0, To: journal.PhaseSearch, Offset: -20, CheckEdge: true, Workloads: []string{w1, w2}})
	a := h.next()
	if a.Kind != RunTrial || a.Trial.Regime != machine.R1 || a.Trial.Workload != w1 {
		t.Fatalf("seeded edge lost frozen R1 class: %+v", a)
	}
	for range h.s.n {
		h.trial(h.next(), passed)
	}
	a = h.next()
	if a.Kind != RunTrial || a.Trial.Regime != machine.R2 || a.Trial.Workload != w2 {
		t.Fatalf("seeded edge lost frozen R2 class: %+v", a)
	}
	for range h.s.n {
		h.trial(h.next(), passed)
	}
	a = h.next()
	p, ok := a.Payload.(*journal.CorePhase)
	if !ok || p.To != journal.PhaseResident || p.Pass == nil || *p.Pass != -20 {
		t.Fatalf("seeded edge did not qualify: %+v", a)
	}
}

func TestSeededCandidateEdgeAdvancesNextWorkloadPair(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseSearch, offset: -20})
	h.add(&journal.CorePhase{Core: 0, To: journal.PhaseSearch, Offset: -20, CheckEdge: true, Workloads: []string{machine.Workloads(machine.R1)[0].ID, machine.Workloads(machine.R2)[0].ID}})
	h.trial(h.next(), failed)
	for range 100 {
		a := h.next()
		if p, ok := a.Payload.(*journal.TunerDecision); ok && p.Decision == journal.CheckEdge {
			want := []string{machine.Workloads(machine.R1)[1].ID, machine.Workloads(machine.R2)[1].ID}
			if diff := cmp.Diff(want, p.Workloads); diff != "" {
				t.Fatalf("next candidate edge workloads (-want +got):\n%s", diff)
			}
			return
		}
		if a.Kind == RunTrial {
			h.trial(a, passed)
		} else {
			h.decide(a)
		}
	}
	t.Fatal("search never scheduled the next candidate edge")
}
