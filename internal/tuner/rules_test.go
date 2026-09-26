package tuner

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/shycler/internal/journal"
)

func search(offset int, pass, fail *int) coreState {
	return coreState{core: 7, phase: journal.PhaseSearch, offset: offset, pass: pass, fail: fail}
}

func confirming(offset int, pass, fail *int) coreState {
	return coreState{core: 7, phase: journal.PhaseConfirmation, offset: offset, pass: pass, fail: fail}
}

func step(from, to int, pass, fail *int, reason string) *journal.TunerDecision {
	return &journal.TunerDecision{Core: 7, Phase: journal.PhaseSearch, Decision: journal.StepDeeper, FromOffset: from, ToOffset: to, Pass: pass, FailedMark: fail, Reason: reason}
}

func backoff(phase journal.Phase, from, to int, pass, fail *int, reason string) *journal.TunerDecision {
	return &journal.TunerDecision{Core: 7, Phase: phase, Decision: journal.Backoff, FromOffset: from, ToOffset: to, Pass: pass, FailedMark: fail, Reason: reason}
}

func toConfirmation(offset int, pass, fail *int, reason string) *journal.CorePhase {
	return &journal.CorePhase{Core: 7, From: journal.PhaseSearch, To: journal.PhaseConfirmation, Offset: offset, Pass: pass, FailedMark: fail, Reason: reason}
}

var deadAtZero = &journal.DeadEnd{Condition: journal.DeadEndFailureAtZero, Core: new(7), Detail: "core 07 failed at CO 0; the instability is not caused by Curve Optimizer"}

func TestRules(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name string
		rule func(coreState) journal.Payload
		in   coreState
		want journal.Payload
	}{
		{"coarse step without failed mark", searchPass, search(-10, nil, nil), step(-10, -15, new(-10), nil, "coarse, no failed mark yet")},
		{"coarse step clamped at the floor", searchPass, search(-47, new(-42), nil), step(-47, -50, new(-47), nil, "coarse, no failed mark yet")},
		{"pass at the floor is the candidate", searchPass, search(-50, new(-45), nil), toConfirmation(-50, new(-50), nil, "candidate edge: -50 is the floor")},
		{"fine step", searchPass, search(-12, new(-11), new(-15)), step(-12, -13, new(-12), new(-15), "fine, failed mark -15")},
		{"fine step reaching the failed mark", searchPass, search(-14, new(-13), new(-15)), toConfirmation(-14, new(-14), new(-15), "candidate edge: -15 is the failed mark")},
		{"failure without pass backs off coarse", failureRule, search(-20, nil, nil), backoff(journal.PhaseSearch, -20, -15, nil, new(-20), "coarse, no passed step")},
		{"coarse backoff clamped at 0", failureRule, search(-3, nil, nil), backoff(journal.PhaseSearch, -3, 0, nil, new(-3), "coarse, no passed step")},
		{"a shallower failure replaces the mark", failureRule, search(-12, nil, new(-15)), backoff(journal.PhaseSearch, -12, -7, nil, new(-12), "coarse, no passed step")},
		{"failure with pass backs off to pass-1", failureRule, search(-15, new(-10), nil), backoff(journal.PhaseSearch, -15, -11, new(-10), new(-15), "fine, deepest pass -10")},
		{"failure next to pass makes it the candidate", failureRule, search(-11, new(-10), new(-15)), toConfirmation(-10, new(-10), new(-11), "candidate edge: -11 is the failed mark")},
		{"contradicted pass is discarded", failureRule, search(-18, new(-20), nil), backoff(journal.PhaseSearch, -18, -13, nil, new(-18), "coarse, no passed step; passed step at -20 discarded, the failure contradicts it")},
		{"search failure at 0 is a dead end", failureRule, search(0, nil, nil), deadAtZero},
		{"confirmation passes", confirmed, confirming(-14, new(-14), new(-15)),
			&journal.CorePhase{Core: 7, From: journal.PhaseConfirmation, To: journal.PhaseConfirmed, Offset: -14, Pass: new(-14), FailedMark: new(-15), Reason: "every R1 and R2 workload, R3, R4 and R5 passed"}},
		{"confirmation failure moves to e+1", failureRule, confirming(-14, new(-14), new(-15)),
			backoff(journal.PhaseConfirmation, -14, -13, nil, new(-14), "confirmation restarts from R1; passed step at -14 discarded, the failure contradicts it")},
		{"confirmation failure keeps a shallower pass", failureRule, confirming(-14, new(-13), new(-15)),
			backoff(journal.PhaseConfirmation, -14, -13, new(-13), new(-14), "confirmation restarts from R1")},
		{"confirmation failure at 0 is a dead end", failureRule, confirming(0, new(0), new(-1)), deadAtZero},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			if diff := cmp.Diff(tt.want, tt.rule(tt.in)); diff != "" {
				t.Fatalf("decision mismatch (-want +got):\n%s", diff)
			}
		})
	}
}

func TestClassifyCrash(t *testing.T) {
	t.Parallel()
	tests := []struct {
		trial, applied bool
		want           CrashKind
	}{
		{true, true, CrashInTrial},
		{true, false, CrashInTrial},
		{false, true, CrashIdle},
		{false, false, CrashStray},
	}
	for _, tt := range tests {
		if got := ClassifyCrash(tt.trial, tt.applied); got != tt.want {
			t.Errorf("ClassifyCrash(%v, %v) = %v, want %v", tt.trial, tt.applied, got, tt.want)
		}
	}
}
