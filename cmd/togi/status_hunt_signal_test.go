package main

import (
	"bytes"
	"strings"
	"testing"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestStatusHuntShowsDecisiveFailureSignal(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name   string
		source journal.Payload
	}{
		{"live failure", &journal.Failure{Signal: machine.Crash}},
		{"live trial", &journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash}},
		{"carried trial", &journal.TrialCarried{Outcome: journal.OutcomeFailure, Signal: machine.Crash}},
		{"carried idle", &journal.FailureCarried{Signal: machine.Crash}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			st := journal.State{
				Session: &journal.SessionInfo{ID: "X"},
				Phase:   string(journal.PhaseHunt),
				Hunt:    &journal.HuntState{Hunt: 1, Seq: 3, Failure: 2, Regime: machine.R7, Trial: "origin"},
			}
			var out bytes.Buffer
			writeStatus(&out, st, []journal.Event{{Seq: 2, Data: tc.source}})
			if !strings.Contains(out.String(), "unattributed crash in R7 trial origin") {
				t.Fatalf("status lost decisive failure signal:\n%s", out.String())
			}
		})
	}
}
