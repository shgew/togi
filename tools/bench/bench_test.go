package main

import (
	"context"
	"errors"
	"math"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/simrun"
)

var approx = cmp.Comparer(func(a, b float64) bool { return math.Abs(a-b) <= 1e-12 })

func TestMetrics(t *testing.T) {
	start := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)
	var events []journal.Event
	add := func(h float64, p journal.Payload) {
		events = append(events, journal.Event{Seq: len(events) + 1, Time: start.Add(time.Duration(h * float64(time.Hour))), Kind: p.Kind(), Data: p})
	}
	add(0, &journal.SessionStart{Cores: []machine.CoreInfo{{Core: 0}, {Core: 1}}})
	add(0, &journal.SessionBaseline{Offsets: []int{0, 0}})
	add(0.1, &journal.CorePhase{Core: 0, To: journal.PhaseAtLimit, Offset: -10})
	add(0.1, &journal.CorePhase{Core: 1, To: journal.PhaseAtLimit, Offset: -11})
	add(0.2, &journal.ProfileChange{To: []int{-10, -11}})
	add(0.3, &journal.TrialEnd{Trial: "1", DurationS: 90, Outcome: journal.OutcomePass})
	add(0.4, &journal.CrashDetected{})
	add(0.5, &journal.TrialEnd{Trial: "2", DurationS: 30, Outcome: journal.OutcomeFailure})
	add(1, &journal.CheckingCycle{Cycle: 1, Event: journal.CycleEnd, Passed: true})
	add(1.1, &journal.CheckingCycle{Cycle: 2, Event: journal.CycleEnd, Passed: true})
	add(1.4, &journal.HuntStart{Hunt: 1, Parked: []int{-10, -11}, Candidates: []int{0, 1}})
	add(1.5, &journal.Combination{Combination: 1, Members: []journal.CombinationMember{{Core: 0, Offset: -12}, {Core: 1, Offset: -12}}})
	add(2, &journal.ProfileApplied{Offsets: []int{0, 0}, Condition: machine.Parked})
	m, err := sim.New(sim.Config{Cores: 2, Limits: []sim.Limits{{Flat: 0.00001, Alone: [5]int{-20, -20, -20, -20, -20}, Together: [7]int{-20, -20, -20, -20, -20, -20, -20}}, {Flat: 0.00002, Alone: [5]int{-20, -20, -20, -20, -20}, Together: [7]int{-20, -20, -20, -20, -20, -20, -20}}}})
	if err != nil {
		t.Fatal(err)
	}
	got := metrics(events, m, 2)
	hazards := map[machine.Regime]float64{}
	for _, regime := range machine.Regimes {
		hazards[regime] = 0.108
	}
	want := result{SimHours: 2, FirstPassedCycleH: new(1.0), PassedCycles: 2, Crashes: 1, Trials: 2, TrialHours: 120.0 / 3600, Hunts: 1, Combinations: 1, FinalProfile: []int{-10, -11}, Depth: -21, HazardPerH: hazards, HazardMaxPerH: 0.108}
	if diff := cmp.Diff(want, got, approx); diff != "" {
		t.Fatalf("metrics (-want +got):\n%s", diff)
	}
	got = metrics(events[:6], m, 2)
	if diff := cmp.Diff((*float64)(nil), got.FirstPassedCycleH); diff != "" {
		t.Fatal(diff)
	}
}

func TestPairing(t *testing.T) {
	a := []result{{Scenario: "b", Seed: 2}, {Scenario: "a", Seed: 9}, {Scenario: "a", Seed: 1}, {Scenario: "a", Seed: 8}}
	b := []result{{Scenario: "a", Seed: 9}, {Scenario: "b", Seed: 2}, {Scenario: "c", Seed: 1}, {Scenario: "a", Seed: 1}}
	want := []pair{{a[2], b[3]}, {a[1], b[0]}, {a[0], b[1]}}
	if diff := cmp.Diff(want, pairing(a, b), cmp.AllowUnexported(pair{})); diff != "" {
		t.Fatal(diff)
	}
}

func TestVerdict(t *testing.T) {
	base := result{Scenario: "default", Seed: 1, Status: "concluded", SimHours: 10, Depth: -20, HazardMaxPerH: 0.1}
	for _, tc := range []struct {
		name       string
		candidate  result
		want       string
		violations [4]int
	}{
		{"equal", base, "NEUTRAL", [4]int{}},
		{"faster", func() result { r := base; r.SimHours = 8; return r }(), "ACCEPT", [4]int{}},
		{"slower", func() result { r := base; r.SimHours = 12; return r }(), "REJECT", [4]int{}},
		{"lost conclusion", func() result { r := base; r.Status = "deadend"; return r }(), "REJECT", [4]int{1, 0, 0, 0}},
		{"censored", func() result { r := base; r.Status = "censored"; r.SimHours = 40; return r }(), "REJECT", [4]int{1, 0, 0, 0}},
		{"hazard", func() result { r := base; r.HazardMaxPerH = 0.111; return r }(), "REJECT", [4]int{0, 1, 0, 0}},
		{"mean depth", func() result { r := base; r.Depth = -18; return r }(), "REJECT", [4]int{0, 0, 1, 0}},
		{"pair depth", func() result { r := base; r.Depth = -14; return r }(), "REJECT", [4]int{0, 0, 2, 0}},
		{"depth boundary", func() result { r := base; r.Depth = -19; return r }(), "NEUTRAL", [4]int{}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			c := compare([]pair{{tc.candidate, base}})
			if diff := cmp.Diff(tc.want, verdict(c)); diff != "" {
				t.Fatal(diff)
			}
			if diff := cmp.Diff(tc.violations, [4]int{c.V1, c.V2, c.V3, c.V4}); diff != "" {
				t.Fatal(diff)
			}
		})
	}
	a, b := base, base
	a.Scenario, b.Scenario = "target", "target"
	a.SimHours = 11
	c := compare([]pair{{a, b}})
	if diff := cmp.Diff([4]int{0, 0, 0, 1}, [4]int{c.V1, c.V2, c.V3, c.V4}); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff("REJECT", verdict(c)); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff("NEUTRAL", verdict(compare(nil))); diff != "" {
		t.Fatal(diff)
	}
	a, b = base, base
	a.Depth = b.Depth + 6
	c = compare([]pair{{a, b}, {base, base}, {base, base}, {base, base}, {base, base}, {base, base}})
	if diff := cmp.Diff(1, c.V3); diff != "" {
		t.Fatal(diff)
	}
	a, b = base, base
	b.HazardMaxPerH = 0
	a.HazardMaxPerH = 0.01
	if diff := cmp.Diff(0, compare([]pair{{a, b}}).V2); diff != "" {
		t.Fatal(diff)
	}
}

func TestScenarioWeightedBootstrap(t *testing.T) {
	pairs := []pair{}
	for seed := range 24 {
		pairs = append(pairs, pair{result{Scenario: "default", Seed: uint64(seed), Status: "concluded", SimHours: 5}, result{Scenario: "default", Seed: uint64(seed), Status: "concluded", SimHours: 10}})
	}
	pairs = append(pairs, pair{result{Scenario: "target", Status: "concluded", SimHours: 20}, result{Scenario: "target", Status: "concluded", SimHours: 10}})
	c := compare(pairs)
	if diff := cmp.Diff([]float64{1, 1, 1}, []float64{c.Ratio, c.Lo, c.Hi}, approx); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff(1, c.V4); diff != "" {
		t.Fatal(diff)
	}
	for i := range pairs {
		pairs[i].candidate.SimHours = 5 + float64(i%3)
	}
	first, second := compare(pairs), compare(pairs)
	if diff := cmp.Diff(first, second); diff != "" {
		t.Fatal(diff)
	}
	if math.IsNaN(first.Lo) || first.Lo > first.Hi {
		t.Fatalf("invalid interval: %+v", first)
	}
}

func TestRunStatus(t *testing.T) {
	for _, tc := range []struct {
		name      string
		exit      int
		events    []journal.Event
		log, want string
	}{
		{"concluded", 0, []journal.Event{{Kind: journal.KindShutdown}}, "", "concluded"},
		{"empty", 0, nil, "", "error"},
		{"deadend journal", 1, []journal.Event{{Kind: journal.KindDeadEnd}}, "", "deadend"},
		{"deadend log", 1, nil, "sim: dead end stock: unstable", "deadend"},
		{"error", 1, nil, "sim: failed to read file", "error"},
		{"boot cap", 3, []journal.Event{{Kind: journal.KindCrashDetected}}, "sim: simulate session: simulated machine reached its boot cap without stopping after 1000 boots", "censored"},
		{"boot cap without journal", 3, nil, "", "error"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if diff := cmp.Diff(tc.want, runStatus(tc.exit, tc.events, tc.log)); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestBootCapRunIsCensoredCost(t *testing.T) {
	t.Parallel()
	m, err := sim.New(sim.Config{Seed: 1})
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	_, err = simrun.Simulate(context.Background(), simrun.Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Cycles: 1, InMemoryJournal: true, MaxBoots: 4})
	if !errors.Is(err, simrun.ErrBootCap) {
		t.Fatalf("capped run: %v, want ErrBootCap", err)
	}
	events, _, err := journal.Read(dir)
	if err != nil {
		t.Fatal(err)
	}
	fresh, err := sim.New(sim.Config{Seed: 1})
	if err != nil {
		t.Fatal(err)
	}
	r := metrics(events, fresh, 16)
	r.Status = runStatus(3, events, "")
	got := struct {
		Status   string
		Crashes  int
		Measured bool
	}{r.Status, r.Crashes, r.SimHours > 0}
	want := struct {
		Status   string
		Crashes  int
		Measured bool
	}{"censored", 3, true}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("capped run record (-want +got):\n%s", diff)
	}
}
