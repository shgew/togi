package tuner

import (
	"slices"
	"testing"

	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
	"github.com/google/go-cmp/cmp"
)

func TestOrder(t *testing.T) {
	t.Parallel()
	var cores []machine.CoreInfo
	for c := 15; c >= 0; c-- {
		cores = append(cores, machine.CoreInfo{Core: c, CCD: c / 8})
	}
	want := []int{0, 8, 1, 9, 2, 10, 3, 11, 4, 12, 5, 13, 6, 14, 7, 15}
	if got := Order(cores); !slices.Equal(got, want) {
		t.Fatalf("Order = %v, want %v", got, want)
	}
	uneven := []machine.CoreInfo{{Core: 0, CCD: 0}, {Core: 1, CCD: 0}, {Core: 2, CCD: 0}, {Core: 5, CCD: 1}}
	if got := Order(uneven); !slices.Equal(got, []int{0, 5, 1, 2}) {
		t.Fatalf("Order(uneven) = %v, want [0 5 1 2]", got)
	}
}

func wantTrial(t *testing.T, a Action, want Trial, cause ...int) {
	t.Helper()
	if a.Kind != RunTrial || a.Trial != want || !slices.Equal(a.Cause, cause) {
		t.Fatalf("action %+v, want trial %+v with cause %v", a, want, cause)
	}
}

func TestSearchSchedule(t *testing.T) {
	t.Parallel()
	h := newHarness(t, searchAt(-10, -10)...)
	search := journal.PhaseSearch

	wantTrial(t, h.s.Next(), Trial{Core: 0, Offset: -10, Regime: machine.R1, Phase: search, Condition: machine.Isolated}, 2)
	_, r1 := h.trial(h.s.Next(), passed)
	wantTrial(t, h.s.Next(), Trial{Core: 0, Offset: -10, Regime: machine.R2, Phase: search, Condition: machine.Isolated}, r1.Seq)
	_, r2 := h.trial(h.s.Next(), passed)

	a := h.s.Next()
	want := &journal.TunerDecision{Core: 0, Phase: search, Decision: journal.StepDeeper, FromOffset: -10, ToOffset: -15, Pass: new(-10), Reason: "coarse, no failed mark yet"}
	if a.Kind != Decide || !slices.Equal(a.Cause, []int{r1.Seq, r2.Seq}) {
		t.Fatalf("after R1+R2 passed: %+v, want a decision with cause [%d %d]", a, r1.Seq, r2.Seq)
	}
	if diff := cmp.Diff(want, a.Payload); diff != "" {
		t.Fatalf("after R1+R2 passed mismatch (-want +got):\n%s", diff)
	}
	decision := h.decide(a)

	wantTrial(t, h.s.Next(), Trial{Core: 1, Offset: -10, Regime: machine.R1, Phase: search, Condition: machine.Isolated}, 3)
	intent, end := h.trial(h.s.Next(), failed)

	a = h.s.Next()
	attributed := &journal.Failure{Signal: machine.ComputationError, Attribution: journal.Attributed, Core: new(1), Offset: new(-10), Trial: "0003"}
	if a.Kind != Decide || !slices.Equal(a.Cause, []int{end.Seq, intent.Seq}) {
		t.Fatalf("after failed trial: %+v, want a decision with cause [%d %d]", a, end.Seq, intent.Seq)
	}
	if diff := cmp.Diff(attributed, a.Payload); diff != "" {
		t.Fatalf("after failed trial mismatch (-want +got):\n%s", diff)
	}
	failure := h.decide(a)
	a = h.s.Next()
	back := &journal.TunerDecision{Core: 1, Phase: search, Decision: journal.Backoff, FromOffset: -10, ToOffset: -5, FailedMark: new(-10), Reason: "coarse, no passed step"}
	if a.Kind != Decide || !slices.Equal(a.Cause, []int{failure.Seq}) {
		t.Fatalf("after failure: %+v, want a decision with cause [%d]", a, failure.Seq)
	}
	if diff := cmp.Diff(back, a.Payload); diff != "" {
		t.Fatalf("after failure mismatch (-want +got):\n%s", diff)
	}
	h.decide(a)

	wantTrial(t, h.s.Next(), Trial{Core: 0, Offset: -15, Regime: machine.R1, Phase: search, Condition: machine.Isolated}, decision.Seq)
	_, inconclusive := h.trial(h.s.Next(), unsure)
	wantTrial(t, h.s.Next(), Trial{Core: 0, Offset: -15, Regime: machine.R1, Phase: search, Condition: machine.Isolated, Retry: true, Workload: "w"}, inconclusive.Seq)
}

func TestConfirmationSchedule(t *testing.T) {
	t.Parallel()
	confirmation := journal.PhaseConfirmation
	h := newHarness(t, coreStart{phase: confirmation, offset: -12}, coreStart{phase: confirmation, offset: -11})
	passes := map[int][]int{}
	for i, sl := range confirmationSet {
		for core, offset := range []int{-12, -11} {
			a := h.s.Next()
			want := Trial{Core: core, Offset: offset, Regime: sl.regime, Phase: confirmation, Condition: machine.Isolated, Workload: sl.workload}
			if a.Kind != RunTrial {
				t.Fatalf("slot %d for core %d: %+v, want a trial", i, core, a)
			}
			if diff := cmp.Diff(want, a.Trial); diff != "" {
				t.Fatalf("slot %d for core %d mismatch (-want +got):\n%s", i, core, diff)
			}
			_, end := h.trial(a, passed)
			passes[core] = append(passes[core], end.Seq)
			if i < len(confirmationSet)-1 {
				continue
			}
			done := h.s.Next()
			if p, ok := done.Payload.(*journal.CorePhase); !ok || p.Core != core || p.To != journal.PhaseConfirmed || !slices.Equal(done.Cause, passes[core]) {
				t.Fatalf("after core %d passed every slot: %+v, want confirmed citing %v", core, done, passes[core])
			}
			h.decide(done)
		}
	}
	if got := len(passes[0]) + len(passes[1]); got != 18 {
		t.Fatalf("%d confirmation trials, want 18", got)
	}
	a := h.s.Next()
	if p, ok := a.Payload.(*journal.ProfileChange); a.Kind != Decide || !ok || p.From != nil || !slices.Equal(p.To, []int{-12, -11}) {
		t.Fatalf("every core confirmed: %+v, want profile.change for guard", a)
	}
	st := projected(h)
	if st.Phase != "guard" || st.Cores[0].Offset != -12 || st.Cores[1].Phase != journal.PhaseConfirmed {
		t.Fatalf("projection %+v", st)
	}
}

func TestConfirmationRestartsFromFirstR1Workload(t *testing.T) {
	t.Parallel()
	confirmation := journal.PhaseConfirmation
	h := newHarness(t, coreStart{phase: confirmation, offset: -12})
	for range 4 {
		h.trial(h.s.Next(), passed)
	}
	a := h.s.Next()
	if a.Trial.Regime != machine.R2 || a.Trial.Workload != machine.Workloads(machine.R2)[1].ID {
		t.Fatalf("fifth trial %+v, want the second R2 workload", a.Trial)
	}
	h.trial(a, failed)
	failure := h.decide(h.s.Next())
	a = h.s.Next()
	back, ok := a.Payload.(*journal.TunerDecision)
	if !ok || back.Decision != journal.Backoff || back.ToOffset != -11 || !slices.Equal(a.Cause, []int{failure.Seq}) {
		t.Fatalf("after the failure: %+v, want a backoff to -11 citing %d", a, failure.Seq)
	}
	h.decide(a)

	r1 := machine.Workloads(machine.R1)
	a = h.s.Next()
	if want := (Trial{Offset: -11, Regime: machine.R1, Phase: confirmation, Condition: machine.Isolated, Workload: r1[0].ID}); a.Trial != want {
		t.Fatalf("after the backoff: %+v, want %+v", a.Trial, want)
	}
	h.trial(a, passed)
	a = h.s.Next()
	if a.Trial.Workload != r1[1].ID {
		t.Fatalf("second trial %+v, want workload %s", a.Trial, r1[1].ID)
	}
	_, inconclusive := h.trial(a, unsure)
	want := Trial{Offset: -11, Regime: machine.R1, Phase: confirmation, Condition: machine.Isolated, Workload: r1[1].ID, Retry: true}
	wantTrial(t, h.s.Next(), want, inconclusive.Seq)
}

func TestConfirmedCoresAreSkipped(t *testing.T) {
	t.Parallel()
	h := newHarness(t, coreStart{phase: journal.PhaseConfirmed, offset: -20}, coreStart{phase: journal.PhaseSearch, offset: -5})
	for range 3 {
		h.trial(h.s.Next(), passed)
		h.trial(h.s.Next(), passed)
		h.decide(h.s.Next())
		if a := h.s.Next(); a.Kind != RunTrial || a.Trial.Core != 1 {
			t.Fatalf("next slot %+v, want core 1", a)
		}
	}
}

func TestFailureAtZeroStaysStopped(t *testing.T) {
	t.Parallel()
	h := newHarness(t, searchAt(0, -10)...)
	h.trial(h.s.Next(), failed)
	failure := h.decide(h.s.Next())
	a := h.s.Next()
	if !slices.Equal(a.Cause, []int{failure.Seq}) {
		t.Fatalf("failure at 0: %+v, want cause [%d]", a, failure.Seq)
	}
	if diff := cmp.Diff(deadAtZero0(), a.Payload); diff != "" {
		t.Fatalf("failure at 0 mismatch (-want +got):\n%s", diff)
	}
	dead := h.decide(a)
	for range 2 {
		a = h.s.Next()
		d, ok := a.Payload.(*journal.DeadEnd)
		if !ok || d.Condition != journal.DeadEndFailureAtZero || *d.Core != 0 || !slices.Equal(a.Cause, []int{dead.Seq}) {
			t.Fatalf("after the dead end: %+v, want it again", a)
		}
	}
	if got := projected(h).Cores[0].FailedMark; got == nil || *got != 0 {
		t.Fatalf("failed mark %v, want 0", got)
	}
}

func TestAttributionPrecedesDeadEnd(t *testing.T) {
	t.Parallel()
	h := newHarness(t, searchAt(0, -10)...)
	h.trial(h.s.Next(), failed)
	h.decide(h.s.Next())
	h.decide(h.s.Next())

	h.trials++
	intent := h.add(&journal.TrialIntent{Trial: "0099", Core: new(1), Offset: new(-10), Regime: machine.R1, Workload: "w", DurationS: 90, Condition: machine.Isolated, Phase: journal.PhaseSearch})
	crash := h.add(&journal.CrashDetected{PreviousBoot: "b", InFlight: new(intent.Seq)})
	end := h.add(&journal.TrialEnd{Trial: "0099", Outcome: journal.OutcomeFailure, Signal: machine.Crash, Reason: "machine crashed during the trial"}, crash.Seq)

	a := h.s.Next()
	f, ok := a.Payload.(*journal.Failure)
	if !ok || *f.Core != 1 || f.Trial != "0099" || !slices.Equal(a.Cause, []int{end.Seq, crash.Seq}) {
		t.Fatalf("with a failing trial awaiting attribution: %+v, want its failure", a)
	}
	b, ok := h.s.Attribution()
	if !ok {
		t.Fatalf("Attribution = %+v, %v; want the same failure", b, ok)
	}
	if diff := cmp.Diff(a, b); diff != "" {
		t.Fatalf("Attribution mismatch (-want +got):\n%s", diff)
	}
	h.decide(a)
	if d, ok := h.s.Next().Payload.(*journal.DeadEnd); !ok || *d.Core != 0 {
		t.Fatalf("after attribution: want the dead end of core 0")
	}
}

func deadAtZero0() *journal.DeadEnd {
	return &journal.DeadEnd{Condition: journal.DeadEndFailureAtZero, Core: new(0), Detail: "core 00 failed at CO 0; the instability is not caused by Curve Optimizer"}
}

func projected(h *harness) journal.State {
	var st journal.State
	journal.Replay(h.events, &st)
	h.s.Project(&st)
	return st
}
