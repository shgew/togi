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
	renderRequests(tab, project(facts.FromEvents(events)), at)
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
