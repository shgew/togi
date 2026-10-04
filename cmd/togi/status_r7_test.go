package main

import (
	"bytes"
	"strings"
	"testing"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestStatusR7SelfSufficiencyAndTopRequester(t *testing.T) {
	w := machine.Workloads(machine.R7)[0].ID
	events := []journal.Event{
		{Seq: 1, Data: &journal.SessionStart{Cores: []machine.CoreInfo{{Core: 3, CCD: 0}, {Core: 7, CCD: 0}}}},
		{Seq: 2, Data: &journal.TrialIntent{Trial: "pass", Regime: machine.R7, Workload: w, Cores: []int{3, 7}, Profile: []int{-20, -30}, DurationS: 120, Condition: machine.Together}},
		{Seq: 3, Data: &journal.TrialEnd{Trial: "pass", Outcome: journal.OutcomePass, DurationS: 120, TopRequesters: []int{3}, VoltageRequestsV: map[int]float64{3: 1.2, 7: 1.1}}},
	}
	var out bytes.Buffer
	writeR7Status(&out, events)
	for _, want := range []string{"R7 self-sufficiency", "not a guarantee", "not yet demonstrated", "TOP REQUESTER", w} {
		if !strings.Contains(out.String(), want) {
			t.Fatalf("missing %q in status:\n%s", want, out.String())
		}
	}
}
