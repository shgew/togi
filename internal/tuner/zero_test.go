package tuner

import (
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

// TestKnownFailureSharingAZeroRerunTrialIsRouted skips a requirement for a carried failure whose source trial ID
// equals a local all-zero rerun's: the skip is routed like a live failure, not taken for the rerun's own failure.
func TestKnownFailureSharingAZeroRerunTrialIsRouted(t *testing.T) {
	h := hasRoomHarness(t, 0, -20)
	w := machine.Workloads(machine.R6)[0].ID
	tr := Trial{Regime: machine.R6, Workload: w, Phase: journal.PhaseChecking, Condition: machine.Together, DurationS: 120, Cores: []int{0, 1}}
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(0)})
	h.decide(h.next())
	rerun, _ := h.trial(h.next(), passed)
	id := rerun.Data.(*journal.TrialIntent).Trial
	if _, ok := h.s.zeroTrials[id]; !ok {
		t.Fatalf("trial %s is not the all-zero rerun", id)
	}
	fact := h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "old", Trial: id}, Class: journal.TrialClass{Regime: machine.R6, Workload: w, Cores: []int{0, 1}, DurationS: 120}, Condition: machine.Together, Profile: []int{0, -19}, Outcome: journal.OutcomeFailure, Signal: machine.Crash})
	a := h.s.skipKnownFailure(Action{Kind: RunTrial, Trial: tr})
	if p, ok := a.Payload.(*journal.Failure); !ok || p.KnownFailure != fact.Seq || p.Trial != id {
		t.Fatalf("skip %+v, want known failure #%d of trial %s", a, fact.Seq, id)
	}
	h.decide(a)
	if c := h.s.core(1); c.pending != fact.Seq {
		t.Fatalf("core 01 pending #%d, want the skipped known failure #%d", c.pending, fact.Seq)
	}
	a, ok := h.s.pendingDecision()
	if back, moved := a.Payload.(*journal.TunerDecision); !ok || !moved || back.Core != 1 || back.ToOffset != -18 {
		t.Fatalf("the skipped known failure did not back off core 01: %+v", a)
	}
	assertProjectionReplay(h)
}

// TestSingleCoreZeroRerunKeepsItsShape reruns a single-core failure at CO 0 as a single-core trial on that core, so
// the rerun loads the same CPUs as the failed trial.
func TestSingleCoreZeroRerunKeepsItsShape(t *testing.T) {
	h := hasRoomHarness(t, 0, -10)
	w := machine.Workloads(machine.R5)[0].ID
	tr := Trial{Core: 0, Regime: machine.R5, Workload: w, Phase: journal.PhaseChecking, Condition: machine.Together, DurationS: 120}
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(0)})
	failure := h.decide(h.next())
	if p := failure.Data.(*journal.Failure); p.Core == nil || *p.Core != 0 || p.Offset == nil || *p.Offset != 0 {
		t.Fatalf("attribution %+v", p)
	}
	a := h.next()
	want := Trial{Core: 0, Regime: machine.R5, Workload: w, DurationS: 120, Condition: machine.Parked, Phase: journal.PhaseChecking, Profile: []int{0, 0}, Rerun: true}
	if diff := cmp.Diff(Action{Kind: RunTrial, Trial: want, Cause: []int{failure.Seq}}, a); diff != "" {
		t.Fatalf("all-zero rerun (-want +got):\n%s", diff)
	}
	intent, _ := h.trial(a, passed)
	if p := intent.Data.(*journal.TrialIntent); p.Core == nil || *p.Core != 0 || len(p.Cores) != 0 || classOf(p) != h.s.failureBySeq(failure.Seq).class {
		t.Fatalf("rerun intent %+v", p)
	}
	a = h.next()
	if start, ok := a.Payload.(*journal.HuntStart); !ok || start.Failure != failure.Seq || !slices.Equal(start.Candidates, []int{1}) {
		t.Fatalf("the failure was not hunted among the nonzero cores after its rerun passed: %+v", a)
	}
	assertProjectionReplay(h)
}
