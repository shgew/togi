package main

import (
	"fmt"
	"path/filepath"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestR7RequestReport(t *testing.T) {
	t.Parallel()
	at := time.Unix(100, 0)
	events := []journal.Event{}
	add := func(regime machine.Regime, workload string, cores []int, duration int, outcome journal.Outcome, v float64, top []int, started bool, when time.Time) {
		id := fmt.Sprint(len(events))
		payloads := []journal.Payload{&journal.TrialIntent{Trial: id, Regime: regime, Workload: workload, Cores: cores, DurationS: duration}}
		if started {
			payloads = append(payloads, &journal.TrialStart{Trial: id})
		}
		end := &journal.TrialEnd{Trial: id, Outcome: outcome, TopRequesters: top}
		if v != 0 {
			end.VoltageRequestsV = map[int]float64{0: v - 0.01, 1: v}
		}
		payloads = append(payloads, end)
		for _, p := range payloads {
			events = append(events, journal.Event{Seq: len(events) + 1, Time: when, Data: p})
		}
	}
	add(machine.R7, "load", []int{1, 0}, 120, journal.OutcomePass, 1.096, []int{1}, true, at)
	add(machine.R7, "load", []int{0, 1}, 120, journal.OutcomeFailure, 1.0999, []int{0, 1}, true, at)
	add(machine.R7, "load", []int{0, 1}, 120, journal.OutcomeInconclusive, 1.1, []int{1}, true, at)
	add(machine.R7, "load", []int{0, 1}, 300, journal.OutcomePass, 1.096, []int{1}, true, at)
	add(machine.R7, "other", []int{0, 1}, 120, journal.OutcomePass, 1.096, []int{1}, true, at)
	add(machine.R7, "load", []int{1}, 120, journal.OutcomePass, 1.096, []int{1}, true, at)
	add(machine.R7, "load", []int{0, 1}, 120, journal.OutcomeFailure, 0, nil, true, at)
	add(machine.R7, "load", []int{0, 1}, 120, journal.OutcomeFailure, 1.096, nil, true, at)
	add(machine.R7, "load", []int{0, 1}, 120, journal.OutcomeFailure, 0, []int{1}, true, at)
	add(machine.R7, "load", []int{0, 1}, 120, journal.OutcomeFailure, 1.096, []int{1}, false, at)
	add(machine.R1, "load", []int{0, 1}, 120, journal.OutcomeFailure, 1.096, []int{1}, true, at)
	add(machine.R7, "load", []int{0, 1}, 120, journal.OutcomeFailure, 1.096, []int{1}, true, at.Add(-time.Second))
	session := facts.FromEvents(events)
	checkGolden(t, "requests.golden", renderTable(t, func(tab *table) {
		renderRequests(tab, computeRequests(r7Measurements(session, project(session), at)))
	}))
}

func TestR7RequestsRecoveredFromSamples(t *testing.T) {
	t.Parallel()
	session, err := facts.ReadJournal(filepath.Join("testdata", "recovered-state", "events.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	const class = "R7/load/00,01/120s"
	want := requestMetrics{
		missingFailures: 1,
		bins: []entry[requestBin, requestCounts]{
			{requestBin{class, 1.128}, requestCounts{trials: 1, failures: 1}},
			{requestBin{class, 1.2}, requestCounts{trials: 1}},
		},
		requesters: []entry[requester, requestCounts]{
			{requester{class, 0}, requestCounts{trials: 1}},
			{requester{class, 1}, requestCounts{trials: 1, failures: 1}},
		},
	}
	if diff := cmp.Diff(want, compute(session, time.Time{}).requests, metricFields); diff != "" {
		t.Errorf("requests (-want +got):\n%s", diff)
	}
}

func TestR7RequestsIncludeCarriedFacts(t *testing.T) {
	t.Parallel()
	at := time.Unix(100, 0)
	class := journal.TrialClass{Regime: machine.R7, Workload: "load", Cores: []int{1, 0}, DurationS: 120}
	carried := func(session string, seq int, outcome journal.Outcome, requests map[int]float64, top []int) journal.Payload {
		return &journal.TrialCarried{Source: journal.FactSource{Session: session, Seq: seq, Trial: fmt.Sprint(seq), Time: at}, Class: class, Condition: machine.Together, Outcome: outcome, VoltageRequestsV: requests, TopRequesters: top}
	}
	measured := map[int]float64{0: 1.1, 1: 1.2}
	payloads := []journal.Payload{
		&journal.SessionStart{Session: "new", Cores: []machine.CoreInfo{{Core: 0}, {Core: 1}}},
		carried("old", 7, journal.OutcomeFailure, measured, []int{1}),
		carried("old", 7, journal.OutcomeFailure, measured, []int{1}),
		carried("older", 7, journal.OutcomePass, measured, []int{1}),
		carried("old", 9, journal.OutcomeFailure, nil, nil),
		carried("new", 9, journal.OutcomeFailure, measured, []int{1}),
		&journal.TrialCarried{Source: journal.FactSource{Session: "old", Seq: 11, Time: at}, Class: journal.TrialClass{Regime: machine.R6, Workload: "load", Cores: []int{0}, DurationS: 120}, Outcome: journal.OutcomeFailure, VoltageRequestsV: measured, TopRequesters: []int{0}},
		&journal.TrialIntent{Trial: "0001", Regime: machine.R7, Workload: "load", Cores: []int{0, 1}, DurationS: 120},
		&journal.TrialStart{Trial: "0001"},
		&journal.TrialEnd{Trial: "0001", Outcome: journal.OutcomeFailure, VoltageRequestsV: measured, TopRequesters: []int{1}},
	}
	events := make([]journal.Event, len(payloads))
	for i, p := range payloads {
		events[i] = journal.Event{Seq: i + 1, Time: at, Data: p}
	}
	const key = "R7/load/00,01/120s"
	want := requestMetrics{
		bins:       []entry[requestBin, requestCounts]{{requestBin{key, 1.2}, requestCounts{trials: 3, failures: 2}}},
		requesters: []entry[requester, requestCounts]{{requester{key, 1}, requestCounts{trials: 3, failures: 2}}},
	}
	if diff := cmp.Diff(want, computeEvents(events, time.Time{}).requests, metricFields); diff != "" {
		t.Errorf("requests (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff(requestMetrics{}, computeEvents(events, at.Add(time.Second)).requests, metricFields); diff != "" {
		t.Errorf("carried facts before --since (-want +got):\n%s", diff)
	}
}
