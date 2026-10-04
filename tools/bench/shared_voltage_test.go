package main

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
)

func voltageMetricConfig() (sim.Config, []int) {
	profile := make([]int, 16)
	for core := range profile {
		profile[core] = -20
	}
	v := &sim.SharedVoltage{
		IdleV: .8, MarginV: .01, Rate: 1.0 / 3600,
		PowerLimitW: 1000, ThermalLimitW: 1000,
		Workload: make(map[string]sim.VoltageWorkload),
	}
	for _, workload := range machine.Workloads(machine.R7) {
		w := sim.VoltageWorkload{ReferenceMHz: 5000, FullMHz: [2]float64{5000, 5000}, WattsPerCore: 1, Core: make([]sim.VoltageCore, 16)}
		for core := range w.Core {
			w.Core[core] = sim.VoltageCore{BaseV: 1.072, ThresholdV: 1.08, CountV: .0036}
		}
		v.Workload[workload.ID] = w
	}
	return sim.Config{Cores: 16, SharedVoltage: v, Limits: make([]sim.Limits, 16), Model: &sim.Model{Signals: map[machine.Signal]float64{machine.ComputationError: 1}}}, profile
}

func voltageMetricMachine(t *testing.T, cfg sim.Config) *sim.Machine {
	t.Helper()
	m, err := sim.New(cfg)
	if err != nil {
		t.Fatal(err)
	}
	return m
}

func TestWorstR7HazardPartial(t *testing.T) {
	cfg, profile := voltageMetricConfig()
	for id, w := range cfg.SharedVoltage.Workload {
		w.Core[0].BaseV = 1.272
		w.Core[1].BaseV = 1.2715
		for core := 8; core < 16; core++ {
			w.Core[core].BaseV = 1.322
		}
		cfg.SharedVoltage.Workload[id] = w
	}
	// An unloaded core's idle hazard must remain in the Machine.Hazard total.
	cfg.Limits[15].Idle = new(-10)
	cfg.Model.PastLimitRate, cfg.Model.Growth = .0002, 1
	m := voltageMetricMachine(t, cfg)
	workload := machine.Workloads(machine.R7)[0]
	partial := machine.TrialSpec{Regime: machine.R7, Workload: workload, Cores: []int{2, 3, 4, 5, 6, 7}}
	want := m.Hazard(profile, partial) * 3600
	full := partial
	full.Cores = []int{0, 1, 2, 3, 4, 5, 6, 7}
	if want <= m.Hazard(profile, full)*3600 {
		t.Fatal("fixture must make the partial more hazardous than its full CCD load")
	}
	events := []journal.Event{
		{Seq: 1, Time: time.Unix(0, 0), Kind: journal.KindSessionStart, Data: &journal.SessionStart{}},
		{Seq: 2, Time: time.Unix(1, 0), Kind: journal.KindSessionBaseline, Data: &journal.SessionBaseline{Offsets: profile}},
	}
	r := metrics(events, m, 16)
	if diff := cmp.Diff(profile, r.FinalProfile); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff(new(want), r.WorstR7HazardPerH, cmpopts.EquateApprox(1e-12, 1e-12)); diff != "" {
		t.Fatal(diff)
	}
	encoded, err := json.Marshal(r)
	if err != nil {
		t.Fatal(err)
	}
	var record map[string]json.RawMessage
	if err := json.Unmarshal(encoded, &record); err != nil {
		t.Fatal(err)
	}
	var reported float64
	if err := json.Unmarshal(record["worst_r7_hazard_per_h"], &reported); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(want, reported); diff != "" {
		t.Fatal(diff)
	}
}

func TestR7PartialChainRecomputesRequestsAndTies(t *testing.T) {
	cfg, profile := voltageMetricConfig()
	for id, w := range cfg.SharedVoltage.Workload {
		for core := range w.Core {
			w.Core[core].BaseV = 1.572
		}
		cfg.SharedVoltage.Workload[id] = w
	}
	workload := machine.Workloads(machine.R7)[0]
	w := cfg.SharedVoltage.Workload[workload.ID]
	w.IdleGainMHz = 100
	values := []float64{1.3, 1.2995, 1.1, 1.14, 1.13, 1.12, 1.11, 1.1}
	for core, request := range values {
		w.Core[core].BaseV = request + .072
	}
	w.Core[2].ClockVPer100MHz = .03
	cfg.SharedVoltage.Workload[workload.ID] = w
	m := voltageMetricMachine(t, cfg)
	spec := machine.TrialSpec{Regime: machine.R7, Workload: workload, Cores: []int{0, 1, 2, 3, 4, 5, 6, 7}}
	var chain [][]int
	for len(spec.Cores) >= 2 {
		chain = append(chain, slices.Clone(spec.Cores))
		lanes, ok := m.R7Requests(profile, spec)
		if !ok {
			t.Fatal("shared-voltage requests unavailable")
		}
		spec.Cores = nextR7Partial(spec.Cores, lanes)
	}
	want := [][]int{{0, 1, 2, 3, 4, 5, 6, 7}, {2, 3, 4, 5, 6, 7}, {3, 4, 5, 6, 7}, {4, 5, 6, 7}, {5, 6, 7}, {6, 7}}
	if diff := cmp.Diff(want, chain); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff([]int{7}, spec.Cores); diff != "" {
		t.Fatal(diff)
	}
	// Never split a tie merely to reach a two-core load.
	spec.Cores = []int{0, 1, 7}
	lanes, _ := m.R7Requests(profile, spec)
	if diff := cmp.Diff([]int{7}, nextR7Partial(spec.Cores, lanes)); diff != "" {
		t.Fatal(diff)
	}
	spec.Cores = []int{6, 7}
	worst := m.Hazard(profile, spec) * 3600
	if diff := cmp.Diff(new(worst), worstR7HazardPerH(m, profile), cmpopts.EquateApprox(1e-12, 1e-12)); diff != "" {
		t.Fatal(diff)
	}
}

func TestWorstR7HazardCCDsAllCoreAndWorkloads(t *testing.T) {
	for _, name := range []string{"second CCD partial", "all-core power pressure", "last workload"} {
		t.Run(name, func(t *testing.T) {
			cfg, profile := voltageMetricConfig()
			workloads := machine.Workloads(machine.R7)
			selected := workloads[0]
			if name == "last workload" {
				selected = workloads[len(workloads)-1]
			}
			for _, workload := range workloads {
				w := cfg.SharedVoltage.Workload[workload.ID]
				for core := range w.Core {
					w.Core[core].BaseV = 1.472
				}
				if name == "all-core power pressure" {
					cfg.SharedVoltage.PowerLimitW = 12
					w.FullMHz = [2]float64{5000, 5200}
					w.PackageMHzPerW = 100
					for core := range w.Core {
						w.Core[core].BaseV = 1.272
						w.Core[core].ClockVPer100MHz = .02
					}
				} else if workload.ID == selected.ID {
					w.Core[8].BaseV = 1.372
					for core := 9; core < 16; core++ {
						w.Core[core].BaseV = 1.072
					}
				}
				cfg.SharedVoltage.Workload[workload.ID] = w
			}
			m := voltageMetricMachine(t, cfg)
			spec := machine.TrialSpec{Regime: machine.R7, Workload: selected, Cores: []int{9, 10, 11, 12, 13, 14, 15}}
			if name == "all-core power pressure" {
				spec.Cores = make([]int, 16)
				for core := range spec.Cores {
					spec.Cores[core] = core
				}
			}
			want := m.Hazard(profile, spec) * 3600
			if diff := cmp.Diff(new(want), worstR7HazardPerH(m, profile), cmpopts.EquateApprox(1e-12, 1e-12)); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestWorstR7OldBaselineCompatibility(t *testing.T) {
	m := voltageMetricMachine(t, sim.Config{Cores: 2})
	r := metrics(nil, m, 2)
	if r.WorstR7HazardPerH != nil {
		t.Fatal("legacy machine gained shared-voltage metric")
	}
	encoded, err := json.Marshal(r)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(encoded), "worst_r7_hazard_per_h") {
		t.Fatal(string(encoded))
	}
	var old result
	if err := json.Unmarshal(encoded, &old); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(r, old); diff != "" {
		t.Fatal(diff)
	}
	base := result{Scenario: "shared", Seed: 1, Status: "concluded", SimHours: 1, HazardMaxPerH: .1}
	candidate := base
	candidate.WorstR7HazardPerH = new(100.0)
	legacy := compare([]pair{{candidate, base}})
	if diff := cmp.Diff(compare([]pair{{base, base}}), legacy); diff != "" {
		t.Fatal(diff)
	}
	base.WorstR7HazardPerH = new(1.0)
	both := compare([]pair{{candidate, base}})
	if diff := cmp.Diff([]float64{1, 99}, []float64{float64(both.WorstR7Pairs), both.MaxWorstR7HazardDelta}); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff("NEUTRAL", verdict(both)); diff != "" {
		t.Fatal(diff)
	}
	// Missing on either side is unavailable, even among measured pairs.
	mixed := compare([]pair{{candidate, base}, {candidate, result{}}, {result{}, base}})
	if diff := cmp.Diff([]float64{1, 99}, []float64{float64(mixed.WorstR7Pairs), mixed.MaxWorstR7HazardDelta}); diff != "" {
		t.Fatal(diff)
	}
	candidate.WorstR7HazardPerH = new(0.0)
	if got := compare([]pair{{candidate, base}}).MaxWorstR7HazardDelta; got != -1 {
		t.Fatalf("negative delta was clamped: %g", got)
	}
}

func TestWorstR7Reports(t *testing.T) {
	rows := []result{
		{Scenario: "shared", Status: "concluded", SimHours: 2, WorstR7HazardPerH: new(.25)},
		{Scenario: "shared", Status: "concluded", SimHours: 2, WorstR7HazardPerH: new(.5)},
		{Scenario: "shared", Status: "concluded", SimHours: 2},
	}
	var got bytes.Buffer
	summaryRow(&got, "shared", rows)
	printComparison(&got, "shared", compare([]pair{{rows[1], rows[0]}}))
	path := filepath.Join("testdata", "worst-r7.golden")
	if *update {
		if err := os.WriteFile(path, got.Bytes(), 0600); err != nil {
			t.Fatal(err)
		}
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(string(want), got.String()); diff != "" {
		t.Fatal(diff)
	}
	var summary, comparison bytes.Buffer
	reportSummary(&summary, rows)
	reportComparison(&comparison, rows[1:2], rows[:1])
	if !strings.Contains(summary.String(), "absent on legacy machines") || !strings.Contains(comparison.String(), "missing is unavailable, not zero") {
		t.Fatalf("missing metric contract: summary=%s comparison=%s", &summary, &comparison)
	}
}
