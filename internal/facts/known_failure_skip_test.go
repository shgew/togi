package facts

import (
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestKnownFailureSkipDoesNotCreateIdleFact(t *testing.T) {
	for _, condition := range []machine.Condition{machine.Together, machine.Parked} {
		t.Run(string(condition), func(t *testing.T) {
			build := journal.Build{Schema: 2, Ruleset: 7}
			at := time.Unix(10, 0).UTC()
			original := journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Condition: condition, Profile: []int{-9, -10}}
			skip := original
			skip.KnownFailure = 2
			s := FromEvents([]journal.Event{
				{Seq: 1, Data: &journal.SessionStart{Build: build, Session: "X", Evidence: 4, Cores: []machine.CoreInfo{{Core: 0}, {Core: 1}}}},
				{Seq: 2, Time: at, Boot: "boot", Data: &original},
				{Seq: 3, Time: at.Add(time.Second), Boot: "boot", Cause: []int{2}, Data: &skip},
			})
			want := []Fact{{Kind: IdleFact, Session: "X", Seq: 2, Time: at, Build: build, Epoch: 4, Boot: "boot", Class: Class{Regime: machine.R6, Cores: []int{0, 1}}, Condition: condition, Profile: original.Profile, Outcome: journal.OutcomeFailure, Signal: machine.Crash, Idle: &IdleContext{Attribution: journal.Unattributed}}}
			if diff := cmp.Diff(want, s.Facts); diff != "" {
				t.Fatalf("skip created a new idle observation (-want +got):\n%s", diff)
			}
		})
	}
}
