package tuner

import (
	"slices"

	"github.com/shgew/togi/internal/machine"
)

// R7CoreStatus describes observed self-sufficiency, not a guarantee against future failures.
type R7CoreStatus struct {
	Core           int
	CCD            int
	Workload       string
	TopRequester   bool
	OffsetFallback bool
	SelfSufficient bool
	Passes         int
}

// R7Status uses the same request order and pass ledger as tuning decisions.
func (s *State) R7Status() []R7CoreStatus {
	if len(s.cores) == 0 {
		return nil
	}
	profile := s.offsets()
	passes := make(map[string]map[int]int)
	for class, entries := range s.ledger {
		if class.regime != machine.R7 {
			continue
		}
		if passes[class.workload] == nil {
			passes[class.workload] = make(map[int]int)
		}
		for _, e := range entries {
			if !e.pass {
				continue
			}
			for _, id := range s.entryTop(e) {
				index, ok := s.indexByID[id]
				if ok && len(e.profile) > index && e.profile[index] <= profile[index] {
					passes[class.workload][id]++
				}
			}
		}
	}
	var out []R7CoreStatus
	for _, w := range machine.Workloads(machine.R7) {
		for _, part := range s.ccdParts() {
			top := s.r7Top(w.ID, part, profile)
			_, sources := s.r7Requests(w.ID, part, profile)
			for _, id := range part {
				status := R7CoreStatus{Core: id, CCD: s.ccd[id], Workload: w.ID, TopRequester: slices.Contains(top, id), OffsetFallback: len(sources) == 0, Passes: passes[w.ID][id]}
				status.SelfSufficient = status.Passes > 0
				out = append(out, status)
			}
		}
	}
	return out
}
