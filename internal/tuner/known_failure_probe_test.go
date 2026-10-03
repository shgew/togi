package tuner

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestKnownFailureSkipCompletesRunningMemberProbeGroup(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseHasRoom, offset: -10}, coreStart{phase: journal.PhaseHasRoom, offset: -10})
	h.decide(h.next())
	tr := Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, DurationS: 120, Condition: machine.Parked, Profile: []int{-9, -10}}
	_, known := h.trial(Action{Kind: RunTrial, Trial: tr}, failed)
	h.decide(h.next())
	tr.Condition, tr.Profile = machine.Together, []int{-10, -10}
	sourceIntent, source := h.trial(Action{Kind: RunTrial, Trial: tr}, failed)
	attribution := h.decide(h.next())
	start := h.add(&journal.HuntStart{Hunt: 1, Failure: attribution.Seq, Trial: sourceIntent.Data.(*journal.TrialIntent).Trial, Regime: tr.Regime, Workload: tr.Workload, Cores: tr.Cores, DurationS: tr.DurationS, StartS: tr.DurationS, Starts: h.s.n, Failing: tr.Profile, Parked: []int{0, 0}, Candidates: tr.Cores})
	h.add(&journal.HuntGroup{Hunt: 1, Group: 1, Stage: "full", Set: tr.Cores, Cores: tr.Cores, Granularity: 2, DurationS: tr.DurationS, Profile: tr.Profile, FullChecked: true, AnyFailed: true, Inferred: "failure"}, start.Seq, source.Seq)
	planned := h.decide(h.next())
	group, ok := planned.Data.(*journal.HuntGroup)
	if !ok || group.Probe == nil || group.Probe.Core != 0 || group.Probe.Offset != -9 || group.Inferred != "" {
		t.Fatalf("expected running first member probe at -9: %+v", planned.Data)
	}
	resolved := h.next()
	replacement, ok := resolved.Payload.(*journal.HuntGroup)
	if !ok || replacement.Group != group.Group || replacement.Inferred != "failure" {
		t.Fatalf("known failure did not resolve the running member-probe group: %+v", resolved)
	}
	if diff := cmp.Diff([]int{planned.Seq, known.Seq}, resolved.Cause); diff != "" {
		t.Fatalf("member probe skip lost original plan or source failure (-want +got):\n%s", diff)
	}
	h.decide(resolved)
	assertProjectionReplay(h)
	continued := h.next()
	probe, ok := continued.Payload.(*journal.HuntGroup)
	if !ok || probe.Probe == nil || probe.Probe.Core != 0 || probe.Probe.Offset != -8 {
		t.Fatalf("inferred member probe failure did not advance to the -8 probe: %+v", continued)
	}
	h.decide(continued)
	for range 100 {
		a := h.next()
		if a.Kind == RunTrial {
			h.trial(a, passed)
			continue
		}
		if _, ok := a.Payload.(*journal.HuntGroup); ok {
			h.decide(a)
			continue
		}
		if end, ok := a.Payload.(*journal.HuntEnd); ok {
			if end.Result != "combination" {
				t.Fatalf("member probe hunt ended as %q, want combination", end.Result)
			}
			want := []journal.CombinationMember{{Core: 0, Offset: -9}, {Core: 1, Offset: -10}}
			if diff := cmp.Diff(want, end.Members); diff != "" {
				t.Fatalf("known member probe failure lost from combination bounds (-want +got):\n%s", diff)
			}
			replayed := New()
			for _, event := range h.events {
				replayed.Fold(event)
			}
			if diff := cmp.Diff(a, replayed.Next()); diff != "" {
				t.Fatalf("resumed member probe result (-live +replayed):\n%s", diff)
			}
			return
		}
		t.Fatalf("unexpected member probe hunt action: %+v", a)
	}
	t.Fatal("member probe hunt did not finish")
}
