package main

import (
	"testing"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestReportR7BackoffsAndChains(t *testing.T) {
	events := []journal.Event{
		{Seq: 1, Data: &journal.SessionStart{Ruleset: 9, Cores: []machine.CoreInfo{{Core: 0}, {Core: 1}}}},
		{Seq: 2, Data: &journal.TrialIntent{Trial: "0001", Regime: machine.R7, Cores: []int{0, 1}}},
		{Seq: 3, Data: &journal.Failure{Trial: "0001", Regime: machine.R7}},
		{Seq: 4, Cause: []int{3}, Data: &journal.TunerDecision{Core: 0, Decision: journal.Backoff, FromOffset: -20, ToOffset: -17, Reason: "request 1.100 V\nto 1.110 V"}},
		{Seq: 5, Data: &journal.CheckingChain{Cycle: 1, Step: 2, CCD: 0, Workload: "avx2", Groups: [][]int{{0}, {1}}, Cores: []int{1}, Part: "partial 1"}},
		{Seq: 6, Data: &journal.TrialIntent{Trial: "0002", Regime: machine.R7, Core: new(1), Cores: []int{1}}},
		{Seq: 7, Data: &journal.Failure{Trial: "0002", Regime: machine.R7}},
		{Seq: 8, Cause: []int{7}, Data: &journal.TunerDecision{Core: 1, Decision: journal.Backoff, FromOffset: -20, ToOffset: -19, Reason: "single-core R7 failure"}},
		{Seq: 9, Data: &journal.CheckingChain{Cycle: 2, Step: 1, CCD: 1, Workload: "sse", Groups: [][]int{{0, 1}}, Cores: []int{0, 1}, Part: "full", SourceSeqs: []int{3, 7}}},
	}
	r := computeR7Decisions(events, time.Time{})
	if len(r.backoffs) != 1 || r.backoffs[0].seq != 4 || r.backoffs[0].steps != 3 {
		t.Fatalf("want only the multi-core voltage-targeted backoff with its counts: %+v", r.backoffs)
	}
	if len(r.chains) != 2 || r.chains[0].seq != 5 || r.chains[1].seq != 9 {
		t.Fatalf("missing chain derivation: %+v", r.chains)
	}
	checkGolden(t, "r7-decisions.golden", renderTable(t, func(tab *table) { renderR7Decisions(tab, r) }))
	events[0].Data = &journal.SessionStart{Ruleset: 8, Cores: []machine.CoreInfo{{Core: 0}, {Core: 1}}}
	if r := computeR7Decisions(events, time.Time{}); len(r.backoffs) != 0 {
		t.Fatalf("ruleset-8 backoffs reported as voltage-targeted: %+v", r.backoffs)
	}
}
