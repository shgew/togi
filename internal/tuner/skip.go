package tuner

import (
	"fmt"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func (s *State) skipKnownFailure(a Action) Action {
	if a.Kind != RunTrial || a.Trial.RecordOnly {
		return a
	}
	t := a.Trial
	profile := t.Profile
	if len(profile) == 0 {
		profile = s.checking.profile
		if t.Condition == machine.Alone {
			profile = make([]int, len(s.cores))
			profile[s.index(t.Core)] = t.Offset
		}
	}
	workload := t.Workload
	if workload == "" {
		workload = machine.PickWorkload(t.Regime, s.core(t.Core).workloadIndex[t.Regime]).ID
	}
	intent := journal.TrialIntent{Regime: t.Regime, Workload: workload, Cores: t.Cores, DurationS: t.DurationS}
	if len(t.Cores) == 0 {
		intent.Core = &t.Core
	}
	k := classOf(&intent)
	seq := s.failingSeq(k, profile, 0)
	if seq == 0 {
		return a
	}
	reason := fmt.Sprintf("skipped %s %s %s trial on %s for %ds at profile %v: known failure #%d at an equal-or-shallower profile is not covered by %d newer passes%s", t.Condition, t.Regime, workload, k.cores, t.DurationS, profile, seq, s.n, s.carriedReason([]int{seq}))
	if t.Condition == machine.Parked {
		h := s.hunt
		m := h.groups[len(h.groups)-1]
		return Action{Kind: Decide, Payload: s.makeGroup(h, planOf(m.payload), m.payload.Group, "failure", false, reason), Cause: []int{m.seq, seq}}
	}
	known := s.failureBySeq(seq)
	if known == nil {
		panic(fmt.Sprintf("tuner: known failure #%d has no attribution", seq))
	}
	failure := *known.failure
	failure.KnownFailure, failure.Reason, failure.Round = seq, reason, t.Round
	failure.Condition, failure.Regime = t.Condition, t.Regime
	if t.Condition == machine.Alone {
		failure.Attribution, failure.Core = journal.Attributed, new(t.Core)
		failure.Offset = new(known.profile[s.index(t.Core)])
	} else if failure.Attribution == journal.Unattributed {
		if i, ok := SoleNonzero(known.profile); ok {
			failure.Attribution = journal.Attributed
			failure.Core, failure.Offset = new(s.byID()[i].id), new(known.profile[i])
		}
	}
	return Action{Kind: Decide, Payload: &failure, Cause: []int{seq}}
}
