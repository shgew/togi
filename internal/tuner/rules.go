package tuner

import (
	"fmt"

	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
)

type coreState struct {
	core   int
	phase  journal.Phase
	offset int
	pass   *int
	fail   *int
}

func searchPass(c coreState) journal.Payload {
	o := c.offset
	pass := new(o)
	switch {
	case o == machine.MinOffset:
		return candidate(c, o, pass, c.fail, fmt.Sprintf("candidate edge: %d is the floor", machine.MinOffset))
	case c.fail == nil:
		return &journal.TunerDecision{Core: c.core, Phase: c.phase, Decision: journal.StepDeeper, FromOffset: o, ToOffset: max(o-5, machine.MinOffset), Pass: pass, Reason: "coarse, no failed mark yet"}
	case o-1 == *c.fail:
		return candidate(c, o, pass, c.fail, fmt.Sprintf("candidate edge: %d is the failed mark", *c.fail))
	}
	return &journal.TunerDecision{Core: c.core, Phase: c.phase, Decision: journal.StepDeeper, FromOffset: o, ToOffset: o - 1, Pass: pass, FailedMark: c.fail, Reason: fmt.Sprintf("fine, failed mark %d", *c.fail)}
}

func failureRule(c coreState) journal.Payload {
	switch c.phase {
	case journal.PhaseSearch:
		return searchFailure(c)
	case journal.PhaseConfirmation, journal.PhaseConfirmed:
	}
	return confirmationFailure(c)
}

func searchFailure(c coreState) journal.Payload {
	o := c.offset
	if o == machine.MaxOffset {
		return failedAtZero(c.core)
	}
	fail := o
	if c.fail != nil {
		fail = max(*c.fail, o)
	}
	pass, discarded := keepPass(c.pass, fail)
	switch {
	case pass == nil:
		return &journal.TunerDecision{Core: c.core, Phase: c.phase, Decision: journal.Backoff, FromOffset: o, ToOffset: min(o+5, machine.MaxOffset), FailedMark: new(fail), Reason: "coarse, no passed step" + discarded}
	case *pass-1 == fail:
		return candidate(c, *pass, pass, new(fail), fmt.Sprintf("candidate edge: %d is the failed mark%s", fail, discarded))
	}
	return &journal.TunerDecision{Core: c.core, Phase: c.phase, Decision: journal.Backoff, FromOffset: o, ToOffset: *pass - 1, Pass: pass, FailedMark: new(fail), Reason: fmt.Sprintf("fine, deepest pass %d%s", *pass, discarded)}
}

func confirmationFailure(c coreState) journal.Payload {
	e := c.offset
	if e == machine.MaxOffset {
		return failedAtZero(c.core)
	}
	pass, discarded := keepPass(c.pass, e)
	return &journal.TunerDecision{Core: c.core, Phase: journal.PhaseConfirmation, Decision: journal.Backoff, FromOffset: e, ToOffset: e + 1, Pass: pass, FailedMark: new(e), Reason: "confirmation restarts from R1" + discarded}
}

func confirmed(c coreState) journal.Payload {
	return &journal.CorePhase{Core: c.core, From: journal.PhaseConfirmation, To: journal.PhaseConfirmed, Offset: c.offset, Pass: c.pass, FailedMark: c.fail, Reason: "R1 to R5 passed"}
}

func candidate(c coreState, offset int, pass, fail *int, reason string) *journal.CorePhase {
	return &journal.CorePhase{Core: c.core, From: journal.PhaseSearch, To: journal.PhaseConfirmation, Offset: offset, Pass: pass, FailedMark: fail, Reason: reason}
}

func keepPass(pass *int, fail int) (*int, string) {
	if pass != nil && *pass <= fail {
		return nil, fmt.Sprintf("; passed step at %d discarded, the failure contradicts it", *pass)
	}
	return pass, ""
}

func failedAtZero(core int) *journal.DeadEnd {
	return &journal.DeadEnd{Condition: journal.DeadEndFailureAtZero, Core: new(core), Detail: fmt.Sprintf("core %02d failed at CO 0; the instability is not caused by Curve Optimizer", core)}
}

func attributeIsolated(intent *journal.TrialIntent, signal machine.Signal) *journal.Failure {
	return &journal.Failure{Signal: signal, Attribution: journal.Attributed, Core: intent.Core, Offset: intent.Offset, Trial: intent.Trial}
}

type CrashKind int

const (
	CrashInTrial CrashKind = iota
	CrashIdle
	CrashStray
)

func ClassifyCrash(trialInFlight, profileApplied bool) CrashKind {
	switch {
	case trialInFlight:
		return CrashInTrial
	case profileApplied:
		return CrashIdle
	}
	return CrashStray
}
