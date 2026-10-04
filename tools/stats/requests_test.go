package main

import (
	"bytes"
	"fmt"
	"os"
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
	var got bytes.Buffer
	tab := &table{out: &got}
	session := facts.FromEvents(events)
	renderRequests(tab, r7Measurements(session, project(session), at))
	if err := tab.writer.Flush(); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join("testdata", "requests.golden")
	if *update {
		if err := os.WriteFile(path, got.Bytes(), 0o644); err != nil {
			t.Fatal(err)
		}
		return
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(string(want), got.String()); diff != "" {
		t.Fatal(diff)
	}
}

const (
	requestsSection  = "R7 voltage requests"
	topSection       = "R7 trials by top request (4 mV bins)"
	requesterSection = "R7 trials by top requester"
)

func TestR7RequestsRecoveredFromSamples(t *testing.T) {
	t.Parallel()
	var out, errOut bytes.Buffer
	if err := run([]string{"--state-dir", filepath.Join("testdata", "recovered-state")}, &out, &errOut); err != nil {
		t.Fatal(err)
	}
	report := out.String()
	if diff := cmp.Diff([][]string{{"failures", "without", "request", "telemetry", "1"}}, sectionRows(t, report, requestsSection)); diff != "" {
		t.Errorf("missing telemetry (-want +got):\n%s", diff)
	}
	wantTop := [][]string{
		{"R7/load/00,01/120s", "[1.128,", "1.132)", "1", "1"},
		{"R7/load/00,01/120s", "[1.200,", "1.204)", "1", "0"},
	}
	if diff := cmp.Diff(wantTop, sectionRows(t, report, topSection)); diff != "" {
		t.Errorf("top requests (-want +got):\n%s", diff)
	}
	wantRequesters := [][]string{
		{"R7/load/00,01/120s", "00", "1", "0"},
		{"R7/load/00,01/120s", "01", "1", "1"},
	}
	if diff := cmp.Diff(wantRequesters, sectionRows(t, report, requesterSection)); diff != "" {
		t.Errorf("top requesters (-want +got):\n%s", diff)
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
	if diff := cmp.Diff([][]string{{"failures", "without", "request", "telemetry", "0"}}, reportRows(t, events, requestsSection)); diff != "" {
		t.Errorf("missing telemetry (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff([][]string{{"R7/load/00,01/120s", "[1.200,", "1.204)", "3", "2"}}, reportRows(t, events, topSection)); diff != "" {
		t.Errorf("top requests (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff([][]string{{"R7/load/00,01/120s", "01", "3", "2"}}, reportRows(t, events, requesterSection)); diff != "" {
		t.Errorf("top requesters (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff([][]string{{"none"}}, reportRows(t, events, topSection, at.Add(time.Second))); diff != "" {
		t.Errorf("carried facts before --since (-want +got):\n%s", diff)
	}
}
