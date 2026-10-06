package trialfacts

import (
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestSpecDescribesClass(t *testing.T) {
	got := Spec(facts.Class{Regime: machine.R7, Workload: "work", Cores: []int{0, 1}, DurationS: 120})
	want := machine.TrialSpec{Regime: machine.R7, Workload: machine.Workload{ID: "work"}, Cores: []int{0, 1}, Duration: 2 * time.Minute}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatal(diff)
	}
}

func TestProfileClassKeyIgnoresProvenanceAndOutcome(t *testing.T) {
	class := facts.Class{Regime: machine.R1, Workload: "work", Cores: []int{0}, DurationS: 60}
	base := Record{Session: "s1", Seq: 1, Kind: facts.TrialFact, Class: class, Condition: machine.Alone, Profile: []int{-10, 0}, Outcome: journal.OutcomePass}
	same := Record{Session: "s2", Seq: 9, Kind: facts.TrialFact, Class: class, Condition: machine.Together, Profile: []int{-10, 0}, Outcome: journal.OutcomeFailure}
	otherProfile := base
	otherProfile.Profile = []int{-11, 0}
	otherClass := base
	otherClass.Class.DurationS = 90
	got := []bool{
		ProfileClassKey(base) == ProfileClassKey(same),
		ProfileClassKey(base) == ProfileClassKey(otherProfile),
		ProfileClassKey(base) == ProfileClassKey(otherClass),
	}
	if diff := cmp.Diff([]bool{true, false, false}, got); diff != "" {
		t.Fatal(diff)
	}
}
