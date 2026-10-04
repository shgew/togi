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
