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
