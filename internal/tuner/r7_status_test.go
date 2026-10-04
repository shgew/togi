package tuner

import (
	"testing"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestR7StatusSeparatesWorkloadsOffsetsAndTopRequesters(t *testing.T) {
	s := New()
	s.Fold(journal.Event{Seq: 1, Data: &journal.SessionStart{Cores: []machine.CoreInfo{{Core: 3, CCD: 0}, {Core: 7, CCD: 0}}}})
	s.cores[0].offset, s.cores[1].offset = -20, -30
	workload := machine.Workloads(machine.R7)[0].ID
	class := trialClass{regime: machine.R7, workload: workload, duration: 120}
	s.ledger[class] = []entry{
		{seq: 2, pass: true, class: class, profile: []int{-21, -30}, cores: []int{3, 7}, top: []int{3}},
		{seq: 3, pass: true, class: class, profile: []int{-19, -29}, cores: []int{3, 7}, top: []int{7}},
	}
	for _, status := range s.R7Status() {
		want := status.Workload == workload && status.Core == 3
		if status.SelfSufficient != want {
			t.Fatalf("self-sufficiency crosses workload, offset or top requester: %+v", status)
		}
		if status.TopRequester != (status.Core == 3) || !status.OffsetFallback {
			t.Fatalf("current offset fallback order: %+v", status)
		}
	}
	s.ledger = map[trialClass][]entry{}
	for _, status := range s.R7Status() {
		if status.SelfSufficient || status.Passes != 0 {
			t.Fatalf("cleared ledger retains self-sufficiency: %+v", status)
		}
	}
}

func TestR7StatusIgnoresFailedStartsAsTopRequester(t *testing.T) {
	for _, carried := range []bool{false, true} {
		t.Run(map[bool]string{false: "live", true: "carried"}[carried], func(t *testing.T) {
			h := r7Harness(t)
			workload := machine.Workloads(machine.R7)[0].ID
			if carried {
				r7Fact(h, false, []int{0, 1}, h.s.Profile(), map[int]float64{0: 1.1, 1: 1.08}, []int{0}, nil, nil, nil)
			} else {
				class := trialClass{regime: machine.R7, workload: workload, cores: "[0 1]", duration: 120}
				h.s.ledger[class] = append(h.s.ledger[class], entry{seq: 99, class: class, profile: h.s.Profile(), cores: []int{0, 1}, top: []int{0}})
			}
			for _, status := range h.s.R7Status() {
				if status.Workload == workload && status.Core == 0 && (status.Passes != 0 || status.SelfSufficient) {
					t.Fatalf("a failed start as top requester counted as self-sufficiency: %+v", status)
				}
			}
		})
	}
}

func TestR7StatusUsesSortedProfileAndOnlyCCDParts(t *testing.T) {
	s := New()
	s.Fold(journal.Event{Seq: 1, Data: &journal.SessionStart{Cores: []machine.CoreInfo{{Core: 7, CCD: 1}, {Core: 3, CCD: 0}, {Core: 9, CCD: 1}, {Core: 5, CCD: 0}}}})
	for _, c := range s.cores {
		c.offset = map[int]int{3: -20, 5: -30, 7: -25, 9: -35}[c.id]
	}
	statuses := s.R7Status()
	if len(statuses) != 4*len(machine.Workloads(machine.R7)) {
		t.Fatalf("all-core part duplicated status rows: %d", len(statuses))
	}
	seen := map[string]map[int]bool{}
	for _, status := range statuses {
		if seen[status.Workload] == nil {
			seen[status.Workload] = map[int]bool{}
		}
		if seen[status.Workload][status.Core] {
			t.Fatalf("duplicate core/workload: %+v", status)
		}
		seen[status.Workload][status.Core] = true
		if status.TopRequester != (status.Core == 3 || status.Core == 7) {
			t.Fatalf("request order used enumeration rather than core-id profile: %+v", status)
		}
	}
}

func TestR7StatusWithoutTopology(t *testing.T) {
	s := New()
	class := trialClass{regime: machine.R7, workload: machine.Workloads(machine.R7)[0].ID}
	s.ledger[class] = []entry{{pass: true, cores: []int{3, 7}, profile: []int{-20, -30}}}
	if got := s.R7Status(); len(got) != 0 {
		t.Fatalf("status invented cores without topology: %+v", got)
	}
}
