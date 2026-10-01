package main

import (
	"bytes"
	"context"
	"flag"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/simrun"
)

var update = flag.Bool("update", false, "rewrite testdata/*.golden from the current output")

func TestSimulatedJointReport(t *testing.T) {
	model := sim.DefaultModel()
	model.PastEdgeRate = 1
	model.Signals = map[machine.Signal]float64{machine.Crash: 1}
	model.CrashMCE = 0
	edges := make([]sim.Edges, 4)
	for i := range edges {
		edges[i].Isolated = [5]int{-50, -50, -50, -50, -50}
		edges[i].Resident = [7]int{-50, -50, -50, -50, -50, -50, -50}
	}
	m, err := sim.New(sim.Config{Seed: 1, Cores: 4, Edges: edges, Model: &model, Joints: []sim.Joint{{Members: map[int]int{1: -10, 3: -10}, Regimes: []machine.Regime{machine.R7}, Rate: 10}}})
	if err != nil {
		t.Fatal(err)
	}
	cfg := config.Default()
	cfg.CandidateEdges = map[int]int{0: -10, 1: -10, 2: -10, 3: -10}
	dir := t.TempDir()
	if _, err := simrun.Simulate(context.Background(), simrun.Input{Config: cfg, ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Rotations: 1}); err != nil {
		t.Fatal(err)
	}
	events, err := readJournal(filepath.Join(dir, "events.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	// Test binaries can carry local version stamps; these are not simulation behavior.
	for _, e := range events {
		if v, ok := e.Data.(*journal.ConfigLoaded); ok {
			v.Version = "test"
			v.Rev = "fixed"
		}
	}
	var got bytes.Buffer
	if err := report(&got, events, time.Time{}); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join("testdata", "joint.golden")
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
		t.Errorf("report mismatch (-want +got):\n%s\nrun `go test ./tools/stats -update` to accept the new output", diff)
	}
}

func TestRunGrouping(t *testing.T) {
	for _, tc := range []struct {
		name  string
		boots []string
		kinds []journal.Payload
		want  []string
	}{
		{"crash reboot continuation", []string{"a", "b", "b", "c", "c"}, []journal.Payload{&journal.ConfigLoaded{}, &journal.ConfigLoaded{}, &journal.CrashDetected{}, &journal.ConfigLoaded{}, &journal.Shutdown{Reason: journal.ShutdownSignal}}, []string{"signal"}},
		{"shutdown splits boots", []string{"a", "a", "b", "b"}, []journal.Payload{&journal.ConfigLoaded{}, &journal.Shutdown{Reason: journal.ShutdownSignal}, &journal.ConfigLoaded{}, &journal.Shutdown{Reason: journal.ShutdownCommand}}, []string{"signal", "command"}},
		{"restart within boot", []string{"a", "a"}, []journal.Payload{&journal.ConfigLoaded{}, &journal.ConfigLoaded{}}, []string{"open", "open"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			events := make([]journal.Event, len(tc.kinds))
			for i, v := range tc.kinds {
				events[i] = journal.Event{Seq: i + 1, Boot: tc.boots[i], Time: time.Unix(int64(i), 0), Data: v}
			}
			p := project(events)
			got := make([]string, len(p.runs))
			for i, r := range p.runs {
				got[i] = r.ending
			}
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestPriorPasses(t *testing.T) {
	target := journal.TrialIntent{Regime: machine.R7, Workload: "load", Cores: []int{1, 0}, DurationS: 120, Profile: []int{-10, -10}, Condition: machine.Masked}
	makeTrial := func(seq int, profile []int, result journal.Outcome, modify func(*journal.TrialIntent)) *trial {
		in := target
		in.Profile = profile
		if modify != nil {
			modify(&in)
		}
		return &trial{intent: &in, key: class(&in, nil), endSeq: seq, end: &journal.TrialEnd{Outcome: result}}
	}
	for _, tc := range []struct {
		name    string
		history []*trial
		want    int
	}{
		{"equal and deeper count", []*trial{makeTrial(1, []int{-10, -10}, journal.OutcomePass, nil), makeTrial(2, []int{-11, -10}, journal.OutcomePass, nil)}, 2},
		{"shallow and incomparable do not", []*trial{makeTrial(1, []int{-9, -10}, journal.OutcomePass, nil), makeTrial(2, []int{-11, -9}, journal.OutcomePass, nil)}, 0},
		{"later shallow failure invalidates", []*trial{makeTrial(1, []int{-11, -11}, journal.OutcomePass, nil), makeTrial(2, []int{-9, -9}, journal.OutcomeFailure, nil), makeTrial(3, []int{-10, -10}, journal.OutcomePass, nil)}, 1},
		{"deeper failure does not invalidate", []*trial{makeTrial(1, []int{-10, -10}, journal.OutcomePass, nil), makeTrial(2, []int{-11, -11}, journal.OutcomeFailure, nil)}, 1},
		{"same sorted cores and different condition", []*trial{makeTrial(1, []int{-10, -10}, journal.OutcomePass, func(in *journal.TrialIntent) { in.Cores = []int{0, 1}; in.Condition = machine.Resident })}, 1},
		{"class fields matter", []*trial{makeTrial(1, []int{-10, -10}, journal.OutcomePass, func(in *journal.TrialIntent) { in.DurationS = 300 }), makeTrial(2, []int{-10, -10}, journal.OutcomePass, func(in *journal.TrialIntent) { in.Workload = "other" }), makeTrial(3, []int{-10, -10}, journal.OutcomePass, func(in *journal.TrialIntent) { in.Regime = machine.R6 }), makeTrial(4, []int{-10, -10}, journal.OutcomePass, func(in *journal.TrialIntent) { in.Cores = []int{0} })}, 0},
		{"hunt boundary excluded", []*trial{makeTrial(10, []int{-10, -10}, journal.OutcomePass, nil), makeTrial(11, []int{-10, -10}, journal.OutcomePass, nil)}, 0},
		{"inconclusive does not invalidate", []*trial{makeTrial(1, []int{-10, -10}, journal.OutcomePass, nil), makeTrial(2, []int{-9, -9}, journal.OutcomeInconclusive, nil)}, 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if diff := cmp.Diff(tc.want, priorPasses(tc.history, &target, 10, nil)); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}
