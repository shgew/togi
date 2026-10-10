package tuner

import (
	"fmt"
	"slices"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

// escalateAfter is the number of voltage-targeted backoffs of one load, since its latest passing trial, after which
// its next unattributed failure is located on the unloaded cores instead of being charged to the loaded cores again.
const escalateAfter = 2

// r7Load identifies a multi-core R7 load whatever its duration: its workload and sorted loaded cores.
type r7Load struct{ workload, cores string }

func loadOf(k trialClass) r7Load { return r7Load{k.workload, k.cores} }

// loadBackoff is one voltage-targeted backoff decision of a load, and the failure that caused it.
type loadBackoff struct{ decision, failure int }

// recordLoadPass forgets the backoffs of a load that just passed: the count restarts at its latest passing trial.
func (s *State) recordLoadPass(k trialClass) {
	delete(s.loads, loadOf(k))
}

// recordLoadBackoff counts a backoff decision against the load of the multi-core R7 failure it answers, unless that
// failure named a core: only unattributed failures are charged by request order and can escalate.
func (s *State) recordLoadBackoff(e journal.Event) {
	for _, seq := range e.Cause {
		f := s.failureBySeq(seq)
		if f == nil || !s.multiR7(f.class) {
			continue
		}
		if loc, ok := s.located[f.seq]; ok && loc.named != nil {
			return
		}
		if failed := s.r7FailureEntry(*f); failed == nil || s.r7NamedCulprit(*failed) {
			return
		}
		if s.loads == nil {
			s.loads = map[r7Load][]loadBackoff{}
		}
		s.loads[loadOf(f.class)] = append(s.loads[loadOf(f.class)], loadBackoff{decision: e.Seq, failure: f.seq})
		return
	}
}

// loadBackoffCount is the number of failures whose backoffs a load had since its latest pass: an all-core failure
// backs off one core per CCD, and still counts once.
func loadBackoffCount(backoffs []loadBackoff) int {
	var seen []int
	for _, b := range backoffs {
		if !slices.Contains(seen, b.failure) {
			seen = append(seen, b.failure)
		}
	}
	return len(seen)
}

// loadedStuckAtZero reports a failure whose affected CCDs have no loaded core off CO 0: backing off loaded cores
// cannot help.
func (s *State) loadedStuckAtZero(failed entry) bool {
	return !slices.ContainsFunc(s.failureTargets(failed), func(id int) bool { return s.r7LoadedMovable(failed, s.ccd[id]) })
}

// escalates reports whether a live unattributed multi-core R7 failure with unloaded cores off CO 0 is located instead
// of backed off: its load already had escalateAfter backoffs since its latest pass, or its loaded cores cannot back off.
func (s *State) escalates(f pendingFailure, failed entry) bool {
	return loadBackoffCount(f.loadBackoffs) >= escalateAfter || s.loadedStuckAtZero(failed)
}

// escalationReason explains why failure f is located, and cites the backoffs that led to it.
func (s *State) escalationReason(f pendingFailure) (string, []int) {
	failed := s.r7FailureEntry(f)
	if failed != nil && s.loadedStuckAtZero(*failed) {
		return "every loaded core of the affected CCDs is at CO 0 while an unloaded core is not, so backing off the loaded cores cannot help", nil
	}
	var cause []int
	var decisions []string
	for _, b := range f.loadBackoffs {
		cause = append(cause, b.decision)
		decisions = append(decisions, fmt.Sprintf("#%d", b.decision))
	}
	return fmt.Sprintf("this load has been backed off %d times since its last passing trial (%v) and still fails, so stepping back its loaded cores is not helping", loadBackoffCount(f.loadBackoffs), decisions), cause
}

// backoffReason explains why a live unattributed multi-core R7 failure with unloaded cores off CO 0 backs off the
// loaded cores instead of being located first.
func (s *State) backoffReason(f pendingFailure, failed entry) string {
	if s.r7NamedCulprit(failed) || f.carried || f.failure.Condition != machine.Together || len(s.locateCandidates(f)) == 0 {
		return ""
	}
	return fmt.Sprintf("not located on the unloaded cores yet: this is voltage-targeted backoff %d of this load since its last passing trial, and its failures are located only after %d", loadBackoffCount(f.loadBackoffs)+1, escalateAfter)
}
