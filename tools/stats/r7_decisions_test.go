package main

import (
	"testing"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestReportR7BackoffsAndChains(t *testing.T) {
	events := []journal.Event{
		{Seq: 1, Data: &journal.SessionStart{Build: journal.Build{Ruleset: 9}, Cores: []machine.CoreInfo{{Core: 0}, {Core: 1}}}},
		{Seq: 2, Data: &journal.TrialIntent{Trial: "0001", Regime: machine.R7, Cores: []int{0, 1}}},
		{Seq: 3, Data: &journal.Failure{Trial: "0001", Regime: machine.R7}},
		{Seq: 4, Cause: []int{3}, Data: &journal.TunerDecision{Core: 0, Decision: journal.Backoff, FromOffset: -20, ToOffset: -17, Reason: "request 1.100 V to 1.110 V"}},
		{Seq: 5, Data: &journal.CheckingChain{Lap: 1, Step: 2, CCD: 0, Workload: "avx2", Groups: [][]int{{0}, {1}}, Cores: []int{1}, Part: "partial 1"}},
		{Seq: 6, Data: &journal.TrialIntent{Trial: "0002", Regime: machine.R7, Core: new(1), Cores: []int{1}}},
		{Seq: 7, Data: &journal.Failure{Trial: "0002", Regime: machine.R7}},
		{Seq: 8, Cause: []int{7}, Data: &journal.TunerDecision{Core: 1, Decision: journal.Backoff, FromOffset: -20, ToOffset: -19, Reason: "single-core R7 failure"}},
	}
	rows := reportRows(t, events, "R7 voltage-targeted backoffs")
	if len(rows) != 1 || rows[0][0] != "4" || rows[0][1] != "backoff" || rows[0][5] != "3" {
		t.Fatalf("want only the multi-core voltage-targeted backoff with its counts: %v", rows)
	}
	chains := reportRows(t, events, "R7 chain derivations")
	if len(chains) != 1 || chains[0][0] != "5" {
		t.Fatalf("missing chain derivation: %v", chains)
	}
	events[0].Data = &journal.SessionStart{Build: journal.Build{Ruleset: 8}, Cores: []machine.CoreInfo{{Core: 0}, {Core: 1}}}
	if rows := reportRows(t, events, "R7 voltage-targeted backoffs"); len(rows) != 0 {
		t.Fatalf("ruleset-8 backoffs reported as voltage-targeted: %v", rows)
	}
}
