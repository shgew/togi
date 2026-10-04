package main

import (
	"testing"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestReportR7BackoffsAndChains(t *testing.T) {
	events := []journal.Event{
		{Seq: 1, Data: &journal.SessionStart{Cores: []machine.CoreInfo{{Core: 0}, {Core: 1}}}},
		{Seq: 2, Data: &journal.Failure{Regime: machine.R7}},
		{Seq: 4, Cause: []int{2}, Data: &journal.TunerDecision{Core: 0, Decision: journal.Backoff, FromOffset: -20, ToOffset: -17, Reason: "request 1.100 V to 1.110 V"}},
		{Seq: 5, Data: &journal.CheckingChain{Lap: 1, Step: 2, CCD: 0, Workload: "avx2", Groups: [][]int{{0}, {1}}, Cores: []int{1}, Part: "partial 1"}},
	}
	rows := reportRows(t, events, "R7 voltage-targeted backoffs")
	if len(rows) != 1 || rows[0][1] != "backoff" || rows[0][5] != "3" {
		t.Fatalf("missing voltage-targeted backoff counts: %v", rows)
	}
	chains := reportRows(t, events, "R7 chain derivations")
	if len(chains) != 1 || chains[0][0] != "5" {
		t.Fatalf("missing chain derivation: %v", chains)
	}
}
