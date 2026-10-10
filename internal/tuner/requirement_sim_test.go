package tuner_test

import (
	"fmt"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/tuner"
)

func TestStoredRequirementMatchesScheduledSimulatedTrials(t *testing.T) {
	for _, run := range []struct {
		seed  uint64
		cores int
	}{{1, 2}, {7, 2}, {1, 16}} {
		t.Run(fmt.Sprintf("%d/%d cores", run.seed, run.cores), func(t *testing.T) {
			s := tuner.New()
			checked := 0
			for _, e := range simulatedForecastJournal(t, run.seed, run.cores) {
				var scheduled tuner.ScheduledRequirement
				p, intent := e.Data.(*journal.TrialIntent)
				if intent {
					a := s.Next()
					if a.Kind != tuner.RunTrial {
						t.Fatalf("trial %s: Next returned %v, not RunTrial", p.Trial, a.Kind)
					}
					scheduled = a.Trial.Requirement
				}
				s.Fold(e)
				if intent {
					if diff := cmp.Diff(scheduled, s.StoredRequirement(p.Trial)); diff != "" {
						t.Fatalf("trial %s (-scheduled +stored):\n%s", p.Trial, diff)
					}
					checked++
				}
			}
			if checked == 0 {
				t.Fatal("simulation recorded no trials")
			}
			t.Logf("matched %d intent-time requirements", checked)
		})
	}
}
