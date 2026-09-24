package tuner

import (
	"reflect"
	"slices"
	"testing"

	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
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
	if a.Kind != Decide || !reflect.DeepEqual(a.Payload, want) || !slices.Equal(a.Cause, []int{r1.Seq, r2.Seq}) {
		t.Fatalf("after R1+R2 passed: %+v, want %+v with cause [%d %d]", a, want, r1.Seq, r2.Seq)
	}
	decision := h.decide(a)

	wantTrial(t, h.s.Next(), Trial{Core: 1, Offset: -10, Regime: machine.R1, Phase: search, Condition: machine.Isolated}, 3)
	intent, end := h.trial(h.s.Next(), failed)

	a = h.s.Next()
	attributed := &journal.Failure{Signal: machine.ComputationError, Attribution: journal.Attributed, Core: new(1), Offset: new(-10), Trial: "0003"}
	if a.Kind != Decide || !reflect.DeepEqual(a.Payload, attributed) || !slices.Equal(a.Cause, []int{end.Seq, intent.Seq}) {
		t.Fatalf("after failed trial: %+v, want %+v with cause [%d %d]", a, attributed, end.Seq, intent.Seq)
	}
	failure := h.decide(a)
	a = h.s.Next()
	back := &journal.TunerDecision{Core: 1, Phase: search, Decision: journal.Backoff, FromOffset: -10, ToOffset: -5, FailedMark: new(-10), Reason: "coarse, no passed step"}
	if a.Kind != Decide || !reflect.DeepEqual(a.Payload, back) || !slices.Equal(a.Cause, []int{failure.Seq}) {
		t.Fatalf("after failure: %+v, want %+v", a, back)
	}
	h.decide(a)

	wantTrial(t, h.s.Next(), Trial{Core: 0, Offset: -15, Regime: machine.R1, Phase: search, Condition: machine.Isolated}, decision.Seq)
	_, inconclusive := h.trial(h.s.Next(), unsure)
	wantTrial(t, h.s.Next(), Trial{Core: 0, Offset: -15, Regime: machine.R1, Phase: search, Condition: machine.Isolated, Retry: true}, inconclusive.Seq)
}

func TestConfirmationSchedule(t *testing.T) {
	t.Parallel()
	confirmation := journal.PhaseConfirmation
	h := newHarness(t, coreStart{phase: confirmation, offset: -12}, coreStart{phase: confirmation, offset: -11})
	for _, r := range machine.ConfirmationRegimes {
		for core, offset := range []int{-12, -11} {
			a := h.s.Next()
			if a.Kind != RunTrial || a.Trial.Core != core || a.Trial.Regime != r || a.Trial.Offset != offset {
				t.Fatalf("slot for %s: %+v, want core %d", r, a, core)
			}
			h.trial(a, passed)
			if core == 1 || r != machine.R5 {
				continue
			}
			done := h.s.Next()
			if p, ok := done.Payload.(*journal.CorePhase); !ok || p.Core != 0 || p.To != journal.PhaseConfirmed || len(done.Cause) != 5 {
				t.Fatalf("after core 0 passed R1-R5: %+v", done)
			}
			h.decide(done)
		}
	}
	done := h.s.Next()
	if p, ok := done.Payload.(*journal.CorePhase); !ok || p.Core != 1 || p.To != journal.PhaseConfirmed {
		t.Fatalf("after core 1 passed R1-R5: %+v", done)
	}
	confirmedAt := h.decide(done)
	a := h.s.Next()
	if p, ok := a.Payload.(*journal.ProfileChange); a.Kind != Decide || !ok || p.From != nil || !slices.Equal(p.To, []int{-12, -11}) || a.Cause[len(a.Cause)-1] != confirmedAt.Seq {
		t.Fatalf("every core confirmed: %+v, want profile.change for guard", a)
	}
	st := projected(h)
	if st.Phase != "guard" || st.Cores[0].Offset != -12 || st.Cores[1].Phase != journal.PhaseConfirmed {
		t.Fatalf("projection %+v", st)
	}
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
	if !reflect.DeepEqual(a.Payload, deadAtZero0()) || !slices.Equal(a.Cause, []int{failure.Seq}) {
		t.Fatalf("failure at 0: %+v", a)
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
	if b, ok := h.s.Attribution(); !ok || !reflect.DeepEqual(b, a) {
		t.Fatalf("Attribution = %+v, %v; want the same failure", b, ok)
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
