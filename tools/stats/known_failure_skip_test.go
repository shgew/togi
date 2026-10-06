package main

import (
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestKnownFailureSkipDoesNotRecountIdleFailure(t *testing.T) {
	original := journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Regime: machine.R6, Condition: machine.Together, Profile: []int{-9, -10}}
	skip := original
	skip.KnownFailure = 2
	intent := func(id string, hunt int) *journal.TrialIntent {
		return &journal.TrialIntent{Trial: id, Hunt: hunt, Group: hunt, Regime: machine.R6, Workload: "load", Cores: []int{0, 1}, DurationS: 120, Condition: machine.Together, Profile: []int{-10, -10}}
	}
	payloads := []journal.Payload{
		&journal.SessionStart{Cores: []machine.CoreInfo{{Core: 0}, {Core: 1}}},
		&original,
		intent("pass", 0),
		&journal.TrialEnd{Trial: "pass", Outcome: journal.OutcomePass},
		&skip,
		&journal.HuntStart{Hunt: 1, Trials: 1},
		&journal.HuntGroup{Hunt: 1, Group: 1, Stage: "part"},
		intent("group", 1),
	}
	var events []journal.Event
	for i, payload := range payloads {
		events = append(events, journal.Event{Seq: i + 1, Time: time.Unix(int64(i), 0), Boot: "boot", Data: payload})
	}
	m := computeEvents(events, time.Time{})
	if diff := cmp.Diff([]entry[failureKey, int]{{failureKey{machine.R6, "-", machine.Crash, journal.Unattributed}, 1}}, m.failures.counts, metricFields); diff != "" {
		t.Fatalf("skip recounted the idle failure (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff(1, m.evidence.groups[0].prior); diff != "" {
		t.Fatalf("skip invalidated a post-source pass (-want +got):\n%s", diff)
	}
}
