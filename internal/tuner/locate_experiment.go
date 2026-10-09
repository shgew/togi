package tuner

import (
	"fmt"
	"slices"

	"github.com/shgew/togi/internal/machine"
)

// loadBackoff is a voltage-targeted backoff caused by an unattributed failure of an R7 load, recorded for
// experiment escalate=K from replayed tuner.decision events.
type loadBackoff struct {
	seq      int
	workload string
	cores    string
}

func (s *State) locatable(f pendingFailure) *entry {
	failed := s.rulesetLocatable(f)
	if failed == nil || !exp.NoLocated {
		return failed
	}
	if s.locateForced(f, *failed) {
		return failed
	}
	if seqs := s.escalationBackoffs(f); exp.Escalate > 0 && len(seqs) >= exp.Escalate {
		return failed
	}
	return nil
}

// locateForced reports a failure whose affected CCDs have no loaded core off CO 0: backing off busy cores
// cannot help, so experiment nolocated keeps its located hunt.
func (s *State) locateForced(f pendingFailure, failed entry) bool {
	return s.loadedAtZero(f) || !slices.ContainsFunc(s.failureTargets(failed), func(id int) bool { return s.r7LoadedMovable(failed, s.ccd[id]) })
}

// escalationBackoffs returns the backoffs that unattributed failures of f's load caused after that load's latest
// pass before f and before f itself.
func (s *State) escalationBackoffs(f pendingFailure) []int {
	since := 0
	for k, entries := range s.ledger {
		if k.regime != machine.R7 || k.workload != f.class.workload || k.cores != f.class.cores {
			continue
		}
		for _, e := range entries {
			if e.pass && e.seq < f.seq {
				since = max(since, e.seq)
			}
		}
	}
	var seqs []int
	for _, b := range s.loadBackoffs {
		if b.workload == f.class.workload && b.cores == f.class.cores && b.seq > since && b.seq < f.seq {
			seqs = append(seqs, b.seq)
		}
	}
	return seqs
}

func (s *State) recordLoadBackoff(seq int, cause []int) {
	if !exp.NoLocated || exp.Escalate == 0 || len(cause) == 0 {
		return
	}
	f := s.failureBySeq(cause[0])
	if f == nil || !s.multiR7(f.class) {
		return
	}
	if loc, ok := s.located[f.seq]; ok && loc.named != nil {
		return
	}
	if failed := s.r7FailureEntry(*f); failed == nil || s.r7NamedCulprit(*failed) {
		return
	}
	s.loadBackoffs = append(s.loadBackoffs, loadBackoff{seq: seq, workload: f.class.workload, cores: f.class.cores})
}

// locateExperimentReason explains why experiment nolocated still starts f's located hunt, and the backoffs an
// escalation cites.
func (s *State) locateExperimentReason(f pendingFailure) (string, []int) {
	if !exp.NoLocated {
		return "", nil
	}
	failed := s.r7FailureEntry(f)
	if failed != nil && s.locateForced(f, *failed) {
		return "every loaded core of each affected CCD was at CO 0, so backing off loaded cores cannot help; located hunt kept (experiment nolocated)", nil
	}
	seqs := s.escalationBackoffs(f)
	return fmt.Sprintf("escalated to a located hunt after %d backoffs of this load %v did not fix it (experiment escalate=%d)", len(seqs), seqs, exp.Escalate), seqs
}

// locateSkippedReason explains a backoff charged directly to the loaded cores where ruleset 10 would have
// located the failure first.
func (s *State) locateSkippedReason(f pendingFailure) string {
	if !exp.NoLocated || s.rulesetLocatable(f) == nil {
		return ""
	}
	if exp.Escalate == 0 {
		return "located hunt skipped (experiment nolocated)"
	}
	return fmt.Sprintf("located hunt skipped after %d of %d backoffs of this load since its last pass (experiment nolocated,escalate=%d)", len(s.escalationBackoffs(f)), exp.Escalate, exp.Escalate)
}
