package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"maps"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/tools/trialfacts"
)

func TestRealAnswerMetrics(t *testing.T) {
	bios := machine.BIOSContext{BIOSVersion: "test"}
	profile := []int{-20, -21}
	replay, err := sim.NewReplay(bios, []sim.ReplayFact{{Context: bios, Class: journal.TrialClass{Regime: machine.R1, Workload: "work", Cores: []int{0}, DurationS: 60}, Profile: profile, Outcome: journal.OutcomePass, DurationS: 60}})
	if err != nil {
		t.Fatal(err)
	}
	m, err := sim.New(sim.Config{Cores: 2, BIOSContext: bios, Replay: replay})
	if err != nil {
		t.Fatal(err)
	}
	start := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)
	var events []journal.Event
	add := func(p journal.Payload) {
		events = append(events, journal.Event{Seq: len(events) + 1, Time: start.Add(time.Duration(len(events)) * time.Second), Kind: p.Kind(), Data: p})
	}
	add(&journal.SessionStart{Session: "s", Cores: []machine.CoreInfo{{Core: 0}, {Core: 1}}})
	add(&journal.SessionBaseline{Offsets: []int{0, 0}})
	for i, tc := range []struct {
		workload string
		profile  []int
		outcome  journal.Outcome
		started  bool
	}{
		{"work", profile, journal.OutcomePass, true},
		{"work", profile, journal.OutcomeFailure, true},
		{"other", profile, journal.OutcomePass, true},
		{"work", []int{-20, -22}, journal.OutcomePass, true},
		{"work", profile, journal.OutcomeInconclusive, true},
		{"work", profile, journal.OutcomeFailure, false},
	} {
		id := fmt.Sprint(i)
		add(&journal.TrialIntent{Trial: id, Profile: tc.profile, Core: new(0), Regime: machine.R1, Workload: tc.workload, DurationS: 60})
		if tc.started {
			add(&journal.TrialStart{Trial: id})
		}
		add(&journal.TrialEnd{Trial: id, Outcome: tc.outcome, DurationS: 60})
	}
	r := metrics(events, m, 2)
	if diff := cmp.Diff(2, r.RealAnswers); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff(1.0/3, r.RealAnswerShare, approx); diff != "" {
		t.Fatal(diff)
	}
}

func TestRealAnswerShares(t *testing.T) {
	rows := []result{{Scenario: "target", Trials: 2, RealAnswers: 1, RealAnswerShare: 0.5}, {Scenario: "target", Trials: 6, RealAnswers: 1, RealAnswerShare: 1.0 / 6}, {Scenario: "empty"}}
	setScenarioShares(rows)
	if diff := cmp.Diff([]float64{0.25, 0.25, 0}, []float64{rows[0].ScenarioRealAnswerShare, rows[1].ScenarioRealAnswerShare, rows[2].ScenarioRealAnswerShare}); diff != "" {
		t.Fatal(diff)
	}
	base := []result{{Scenario: "target", Trials: 10, RealAnswers: 1}, {Scenario: "target", Trials: 10, RealAnswers: 3}}
	c := compare([]pair{{rows[0], base[0]}, {rows[1], base[1]}})
	if diff := cmp.Diff([]float64{0.25, 0.2}, []float64{c.RealAnswerShare, c.BaselineRealAnswerShare}); diff != "" {
		t.Fatal(diff)
	}
	encoded, err := json.Marshal(rows[0])
	if err != nil {
		t.Fatal(err)
	}
	var decoded map[string]any
	if err := json.Unmarshal(encoded, &decoded); err != nil {
		t.Fatal(err)
	}
	got := []any{decoded["real_answers"], decoded["real_answer_share"], decoded["scenario_real_answer_share"]}
	if diff := cmp.Diff([]any{float64(1), 0.5, 0.25}, got); diff != "" {
		t.Fatal(diff)
	}
	var comparison, summary bytes.Buffer
	printComparison(&comparison, "target", c)
	summaryRow(&summary, "target", rows[:2])
	if diff := cmp.Diff(true, strings.Contains(comparison.String(), "real_answer_share=0.250000 baseline_real_answer_share=0.200000")); diff != "" {
		t.Fatal(diff)
	}
	columns := strings.Fields(summary.String())
	if diff := cmp.Diff("0.250000", columns[len(columns)-1]); diff != "" {
		t.Fatal(diff)
	}
}

func TestTargetSuite(t *testing.T) {
	extracts := trialfacts.Extracts{}
	runs, err := loadRuns("suite.json", "all", extracts)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]string{filepath.Join("facts", "target.jsonl.gz")}, slices.Collect(maps.Keys(extracts))); diff != "" {
		t.Fatal(diff)
	}
	type splitCounts struct{ Dev, Holdout int }
	members := map[string]splitCounts{}
	for _, run := range runs {
		if run.scenario.Name != "target" {
			continue
		}
		if diff := cmp.Diff(true, run.scenario.Replay && run.cfg.Replay != nil); diff != "" {
			t.Fatal(diff)
		}
		name := filepath.Base(run.scenario.Machine)
		c := members[name]
		if run.split == "dev" {
			c.Dev++
		} else {
			c.Holdout++
		}
		members[name] = c
	}
	want := map[string]splitCounts{}
	for i := range 9 {
		want[fmt.Sprintf("target-fit-%d.json", i)] = splitCounts{2, 2}
	}
	if diff := cmp.Diff(want, members); diff != "" {
		t.Fatal(diff)
	}
}
