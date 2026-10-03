package main

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
)

func TestPartialMetrics(t *testing.T) {
	start := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)
	var events []journal.Event
	add := func(p journal.Payload) {
		events = append(events, journal.Event{Seq: len(events) + 1, Time: start.Add(time.Duration(len(events)) * time.Minute), Kind: p.Kind(), Data: p})
	}
	add(&journal.SessionStart{Cores: []machine.CoreInfo{{Core: 0}, {Core: 1}}})
	add(&journal.SessionBaseline{Offsets: []int{0, 0}})
	trial := func(id string, lap int, recordOnly bool, outcome journal.Outcome, elapsed int) {
		add(&journal.TrialIntent{Trial: id, Profile: []int{0, 0}, Cores: []int{0}, Regime: machine.R7, Workload: "partial", DurationS: 120, Condition: machine.Together, Phase: journal.PhaseChecking, Lap: lap, RecordOnly: recordOnly})
		add(&journal.TrialEnd{Trial: id, Outcome: outcome, DurationS: elapsed})
	}
	trial("pass", 1, true, journal.OutcomePass, 120)
	trial("failure", 1, true, journal.OutcomeFailure, 7)
	trial("inconclusive", 1, true, journal.OutcomeInconclusive, 3)
	trial("full", 1, false, journal.OutcomePass, 120)
	trial("outside-lap", 0, true, journal.OutcomePass, 120)
	add(&journal.TrialEnd{Trial: "no-intent", Outcome: journal.OutcomePass, DurationS: 120})
	add(&journal.CheckingLap{Lap: 1, Event: journal.LapEnd, Passed: true})
	trial("second", 2, true, journal.OutcomeFailure, 20)
	add(&journal.CheckingLap{Lap: 2, Event: journal.LapEnd, Passed: true})
	trial("dirty", 3, true, journal.OutcomePass, 99)
	add(&journal.CheckingLap{Lap: 3, Event: journal.LapEnd})
	trial("unfinished-lap", 4, true, journal.OutcomePass, 50)
	add(&journal.CheckingLap{Lap: 4, Event: journal.LapStart, Passed: true})
	add(&journal.TrialIntent{Trial: "unfinished-trial", Lap: 4, RecordOnly: true})
	add(&journal.CrashDetected{})
	m, err := sim.New(sim.Config{Cores: 2})
	if err != nil {
		t.Fatal(err)
	}
	r := metrics(events, m, 2)
	if diff := cmp.Diff([]float64{2, 150, 75}, []float64{float64(r.PassedLaps), r.PartialSeconds, partialSecondsPerPassedLap(r.PartialSeconds, r.PassedLaps)}); diff != "" {
		t.Fatal(diff)
	}
	encoded, err := json.Marshal(r)
	if err != nil {
		t.Fatal(err)
	}
	var record map[string]any
	if err := json.Unmarshal(encoded, &record); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]any{float64(2), float64(150)}, []any{record["passed_laps"], record["partial_seconds"]}); diff != "" {
		t.Fatal(diff)
	}
}

func TestPartialComparisonOldBaseline(t *testing.T) {
	path := filepath.Join(t.TempDir(), "baseline.jsonl")
	if err := os.WriteFile(path, []byte("{\"scenario\":\"partial\",\"seed\":1,\"split\":\"dev\",\"status\":\"concluded\",\"sim_hours\":10}\n"), 0600); err != nil {
		t.Fatal(err)
	}
	baseline, err := readResults(path)
	if err != nil {
		t.Fatal(err)
	}
	candidate := append([]result(nil), baseline...)
	candidate[0].PassedLaps, candidate[0].PartialSeconds = 2, 150
	c := compare(pairing(candidate, baseline))
	if diff := cmp.Diff([]float64{75, 0}, []float64{c.PartialSecondsPerPassedLap, c.BaselinePartialSecondsPerPassedLap}); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff("NEUTRAL", verdict(c)); diff != "" {
		t.Fatal(diff)
	}
	var comparison, summary bytes.Buffer
	reportComparison(&comparison, candidate, baseline)
	if !strings.Contains(comparison.String(), "partial_s_per_passed_lap=75.000 baseline_partial_s_per_passed_lap=0.000") {
		t.Fatal(comparison.String())
	}
	reportSummary(&summary, baseline)
	if !strings.HasSuffix(summary.String(), "                  0.000\n") {
		t.Fatal(summary.String())
	}
	if got := partialSecondsPerPassedLap(0, 0); got != 0 {
		t.Fatalf("no passed laps: %v", got)
	}
}
