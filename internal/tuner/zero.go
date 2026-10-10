package tuner

import (
	"fmt"
	"slices"
	"strings"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

// zeroRerun is the all-zero rerun of a failing trial that would otherwise end the session at CO 0: its trial,
// and once it ended conclusively, its trial.end sequence and outcome.
type zeroRerun struct {
	trial  string
	end    int
	passed bool
}

const notCurveOptimizer = "the instability is not caused by Curve Optimizer"

// atZero returns what a failure_at_zero dead end needs first. A failing profile already all at CO 0 dead-ends at
// once; otherwise the failing trial reruns with every core at 0, and dead-ends citing both failures if that fails.
// It reports nothing once the rerun passed: the failure then goes to the nonzero cores.
func (s *State) atZero(f pendingFailure, dead *journal.DeadEnd, cause []int) (Action, bool) {
	if allZero(f.profile) {
		return Action{Kind: Decide, Payload: dead, Cause: cause}, true
	}
	r := s.zeroReruns[f.seq]
	switch {
	case r == nil || r.end == 0:
		return s.zeroRerunTrial(f), true
	case r.passed:
		return Action{}, false
	}
	detail, _ := strings.CutSuffix(dead.Detail, "; "+notCurveOptimizer)
	dead.Detail = fmt.Sprintf("%s; the rerun with every core at CO 0 failed too (#%d), so %s", detail, r.end, notCurveOptimizer)
	return Action{Kind: Decide, Payload: dead, Cause: append(slices.Clone(cause), r.end)}, true
}

// zeroRerunTrial reruns the failed trial with every core at CO 0 in its own shape: a single-core trial stays one,
// so the rerun loads the same CPUs as the failure it confirms.
func (s *State) zeroRerunTrial(f pendingFailure) Action {
	if s.retry != nil && s.retry.Rerun && s.retry.Condition == machine.Parked {
		return s.retryTrial(*s.retry, []int{f.seq})
	}
	phase := journal.PhaseChecking
	if f.failure.Condition == machine.Parked {
		phase = journal.PhaseHunt
	}
	t := Trial{Regime: f.class.regime, Workload: f.class.workload, DurationS: f.class.duration, Condition: machine.Parked, Phase: phase, Profile: make([]int, len(s.cores)), Rerun: true}
	var intent *journal.TrialIntent
	if !f.carried {
		intent = s.intents[f.failure.Trial]
	}
	target := s.classTargets[f.class.cores]
	switch {
	case intent != nil && intent.Core != nil && len(intent.Cores) == 0:
		t.Core = *intent.Core
	case intent == nil && !target.multi && len(target.cores) == 1:
		t.Core = target.cores[0]
	default:
		t.Cores = slices.Clone(s.classCores(f.class))
		if len(t.Cores) == 0 {
			t.Cores = s.ids()
		}
	}
	return s.runTrial(t, s.rerunRequirement(s.shapeClass(t)), []int{f.seq})
}

// recordZeroRerun remembers an all-zero rerun by the failure its intent cites.
func (s *State) recordZeroRerun(e journal.Event, p *journal.TrialIntent) {
	if p.Condition != machine.Parked || !p.Rerun || p.Hunt != 0 || len(e.Cause) == 0 {
		return
	}
	if s.zeroReruns == nil {
		s.zeroReruns, s.zeroTrials = map[int]*zeroRerun{}, map[string]int{}
	}
	s.zeroReruns[e.Cause[0]] = &zeroRerun{trial: p.Trial}
	s.r7Epoch++
	s.zeroTrials[p.Trial] = e.Cause[0]
}

// endZeroRerun records a conclusive all-zero rerun. A pass sends the failure to the nonzero cores: a core named at
// CO 0 outside multi-core R7 no longer owns it, and a failure that was not a hunt group's queues a hunt.
func (s *State) endZeroRerun(e journal.Event, p *journal.TrialEnd) {
	seq, ok := s.zeroTrials[p.Trial]
	if !ok || p.Outcome != journal.OutcomePass && p.Outcome != journal.OutcomeFailure {
		return
	}
	r := s.zeroReruns[seq]
	r.end, r.passed = e.Seq, p.Outcome == journal.OutcomePass
	s.r7Epoch++
	f := s.failureBySeq(seq)
	if !r.passed || f == nil || s.multiR7(f.class) {
		return
	}
	if f.failure.Core != nil {
		if c := s.core(*f.failure.Core); c != nil && c.pending == seq {
			c.pending = 0
		}
	}
	if f.failure.Condition != machine.Parked {
		s.queue = append(s.queue, *f)
	}
	s.projectionDirty = true
}

// zeroRerunFailure reports a failed all-zero rerun, whose failure only answers its rerun. A known-failure skip
// carries its known failure's trial, which a carried failure took from another session, so it never matches.
func (s *State) zeroRerunFailure(p *journal.Failure) bool {
	_, ok := s.zeroTrials[p.Trial]
	return ok && p.Trial != "" && p.KnownFailure == 0
}
