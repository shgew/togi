package tuner

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestKnownFailureSkipCompletesRunningEdgeMask(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseResident, offset: -10}, coreStart{phase: journal.PhaseResident, offset: -10})
	h.decide(h.next())
	tr := Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, DurationS: 120, Condition: machine.Masked, Profile: []int{-9, -10}}
	_, known := h.trial(Action{Kind: RunTrial, Trial: tr}, failed)
	h.decide(h.next())
	tr.Condition, tr.Profile = machine.Resident, []int{-10, -10}
	sourceIntent, source := h.trial(Action{Kind: RunTrial, Trial: tr}, failed)
	attribution := h.decide(h.next())
	start := h.add(&journal.HuntStart{Hunt: 1, Failure: attribution.Seq, Trial: sourceIntent.Data.(*journal.TrialIntent).Trial, Regime: tr.Regime, Workload: tr.Workload, Cores: tr.Cores, DurationS: tr.DurationS, StartS: tr.DurationS, Starts: h.s.n, Failing: tr.Profile, Anchor: []int{0, 0}, Candidates: tr.Cores})
	h.add(&journal.HuntMask{Hunt: 1, Mask: 1, Stage: "full", Set: tr.Cores, Cores: tr.Cores, Granularity: 2, DurationS: tr.DurationS, Profile: tr.Profile, FullChecked: true, AnyFailed: true, Inferred: "failure"}, start.Seq, source.Seq)
	planned := h.decide(h.next())
	mask, ok := planned.Data.(*journal.HuntMask)
	if !ok || mask.Edge == nil || mask.Edge.Core != 0 || mask.Edge.Offset != -9 || mask.Inferred != "" {
		t.Fatalf("expected running first edge probe at -9: %+v", planned.Data)
	}
	resolved := h.next()
	replacement, ok := resolved.Payload.(*journal.HuntMask)
	if !ok || replacement.Mask != mask.Mask || replacement.Inferred != "failure" {
		t.Fatalf("known failure did not resolve the running edge mask: %+v", resolved)
	}
	if diff := cmp.Diff([]int{planned.Seq, known.Seq}, resolved.Cause); diff != "" {
		t.Fatalf("edge skip lost original plan or source failure (-want +got):\n%s", diff)
	}
	h.decide(resolved)
	assertProjectionReplay(h)
	continued := h.next()
	probe, ok := continued.Payload.(*journal.HuntMask)
	if !ok || probe.Edge == nil || probe.Edge.Core != 0 || probe.Edge.Offset != -8 {
		t.Fatalf("inferred edge failure did not advance to the -8 probe: %+v", continued)
	}
	h.decide(continued)
	for range 100 {
		a := h.next()
		if a.Kind == RunTrial {
			h.trial(a, passed)
			continue
		}
		if _, ok := a.Payload.(*journal.HuntMask); ok {
			h.decide(a)
			continue
		}
		if end, ok := a.Payload.(*journal.HuntEnd); ok {
			if end.Result != "joint" {
				t.Fatalf("edge hunt ended as %q, want joint", end.Result)
			}
			want := []journal.JointMember{{Core: 0, Offset: -9}, {Core: 1, Offset: -10}}
			if diff := cmp.Diff(want, end.Members); diff != "" {
				t.Fatalf("known edge failure lost from joint bounds (-want +got):\n%s", diff)
			}
			replayed := New()
			for _, event := range h.events {
				replayed.Fold(event)
			}
			if diff := cmp.Diff(a, replayed.Next()); diff != "" {
				t.Fatalf("resumed edge result (-live +replayed):\n%s", diff)
			}
			return
		}
		t.Fatalf("unexpected edge hunt action: %+v", a)
	}
	t.Fatal("edge hunt did not finish")
}
