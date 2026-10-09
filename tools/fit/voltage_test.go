package main

import (
	"math"
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/tools/modelcheck"
	"github.com/shgew/togi/tools/trialfacts"
)

func voltageRecords() []trialfacts.Record {
	var records []trialfacts.Record
	for i := range 40 {
		profile := slices.Repeat([]int{-35}, 16)
		mhz := 5200 + (i%4)*20
		requests := make(map[int]float64)
		for core := range 8 {
			profile[core] = -20 - i/4
			requests[core] = 1.22 + float64(core)*.005 + .0038*float64(profile[core]) + .03*float64(mhz-5240)/100
		}
		records = append(records, trialfacts.Record{Kind: facts.TrialFact, Profile: profile, Outcome: journal.OutcomePass, Class: facts.Class{Regime: machine.R7, Workload: machine.PickWorkload(machine.R7, 0).ID, Cores: []int{0, 1, 2, 3, 4, 5, 6, 7}, DurationS: 120}, VoltageRequestsV: requests, CCDMHz: map[int]int{0: mhz}})
	}
	return records
}

func TestSharedVoltageRequestRegression(t *testing.T) {
	records := voltageRecords()
	v := voltageConfig(records)
	w := v.Workload[machine.PickWorkload(machine.R7, 0).ID]
	for core := range 8 {
		if math.Abs(w.Core[core].CountV-.0038) > .00001 || math.Abs(w.Core[core].ClockVPer100MHz-.03) > .0001 {
			t.Fatalf("regression core %d: %+v", core, w.Core[core])
		}
		if w.Core[core].ThresholdClockVPer100MHz != w.Core[core].ClockVPer100MHz {
			t.Fatal("required voltage clock slope must equal the measured request slope")
		}
	}
	b := v.Workload[machine.PickWorkload(machine.R7, 1).ID]
	if diff := cmp.Diff(w.Core, b.Core); diff != "" {
		t.Fatalf("unmeasured workload must inherit the request curve: %s", diff)
	}
	if diff := cmp.Diff([2]float64{5230, 5240}, w.FullMHz); diff != "" {
		t.Fatalf("clock medians / unsupported prior: %s", diff)
	}
	if diff := cmp.Diff(v, voltageConfig(records)); diff != "" {
		t.Fatalf("request fit is not deterministic: %s", diff)
	}
}

func TestSharedVoltageNamedFailureLikelihood(t *testing.T) {
	records := voltageRecords()
	cfg := initialConfig(records)
	cfg.Joints, cfg.CCD, cfg.SharedVoltage = nil, nil, voltageConfig(records)
	w := cfg.SharedVoltage.Workload[machine.PickWorkload(machine.R7, 0).ID]
	for core := range w.Core {
		w.Core[core].ThresholdV = 1.0
	}
	w.Core[0].ThresholdV = 1.2
	failure := records[0]
	failure.Outcome, failure.Signal, failure.Core = journal.OutcomeFailure, machine.ComputationError, new(0)
	var l voltageLikelihood
	l.cfg, l.obs, l.named = cfg, aggregate([]trialfacts.Record{failure}), namedVoltageFailures([]trialfacts.Record{failure})
	l.rebuild()
	first := l.score([]int{0})
	failure.Core = new(1)
	l.named = namedVoltageFailures([]trialfacts.Record{failure})
	if other := l.score([]int{0}); other <= first+5 {
		t.Fatalf("named-core attribution did not constrain the named hazard: core0=%g core1=%g", first, other)
	}
	failure.Core, failure.StalledCore = nil, new(0)
	if diff := cmp.Diff(0, namedVoltageFailures([]trialfacts.Record{failure})[0].core); diff != "" {
		t.Fatal(diff)
	}
	failure.StalledCore = new(15)
	if len(namedVoltageFailures([]trialfacts.Record{failure})) != 0 {
		t.Fatal("an unloaded core must not become a loaded-core failure attribution")
	}
}

func TestSharedVoltageFitDeterminismAndIsolation(t *testing.T) {
	records := voltageRecords()
	before := voltageConfig(records)
	cfg, loss := fitSharedVoltage(records)
	again, againLoss := fitSharedVoltage(records)
	if diff := cmp.Diff(cfg, again); diff != "" {
		t.Fatalf("fixed-input fit differs: %s", diff)
	}
	if diff := cmp.Diff(loss, againLoss); diff != "" {
		t.Fatalf("fixed-input loss differs: %s", diff)
	}
	if cfg.SharedVoltage == nil || cfg.CCD != nil || len(cfg.Joints) != 0 {
		t.Fatal("in-sample shared fit must replace joints and CCD residual")
	}
	legacy := initialConfig(records)
	if legacy.SharedVoltage != nil {
		t.Fatal("legacy fitter initialization changed")
	}
	if diff := cmp.Diff(before, voltageConfig(records)); diff != "" {
		t.Fatalf("fitting mutated input telemetry: %s", diff)
	}
	m, err := sim.NewPredictor(cfg)
	if err != nil {
		t.Fatal(err)
	}
	l := likelihood{cfg: cfg, m: m, obs: aggregate(records)}
	indices := l.selectObs(func(observation) bool { return true })
	if diff := cmp.Diff(l.rawScore(indices), loss); diff != "" {
		t.Fatalf("reported likelihood must exclude priors and named-core terms: %s", diff)
	}
	for _, workload := range machine.Workloads(machine.R7) {
		w := cfg.SharedVoltage.Workload[workload.ID]
		for _, core := range w.Core {
			if core.ThresholdV < .8 || core.ThresholdV > 1.5 || core.BaseV < .9 || core.BaseV > 1.5 || core.CountV < .0025 || core.CountV > .005 || core.ThresholdClockVPer100MHz != core.ClockVPer100MHz {
				t.Fatalf("fit outside physical bounds: %+v", core)
			}
		}
	}
}

func TestSharedVoltageBackgroundExplainsStockFailure(t *testing.T) {
	var records []trialfacts.Record
	for i := range 149 {
		offset := 0
		if i >= 29 {
			offset = -30
		}
		outcome := journal.OutcomePass
		if i == 0 {
			outcome = journal.OutcomeFailure
		}
		records = append(records, trialfacts.Record{
			Kind: facts.TrialFact, Profile: slices.Repeat([]int{offset}, 16), Outcome: outcome,
			Class: facts.Class{Regime: machine.R7, Workload: machine.PickWorkload(machine.R7, 1).ID,
				Cores: []int{0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15}, DurationS: 600},
		})
	}
	cfg, _ := fitSharedVoltage(records)
	if cfg.SharedVoltage.BackgroundRate <= 0 || cfg.SharedVoltage.BackgroundRate > .001 {
		t.Fatalf("stock failure must identify a bounded background rate, got %g", cfg.SharedVoltage.BackgroundRate)
	}
	m, err := sim.New(cfg)
	if err != nil {
		t.Fatal(err)
	}
	spec := machine.TrialSpec{Regime: machine.R7, Workload: machine.PickWorkload(machine.R7, 1), Cores: records[0].Class.Cores}
	background := cfg.SharedVoltage.BackgroundRate
	if stock := m.Hazard(records[0].Profile, spec); stock < background {
		t.Fatalf("stock hazard %g omitted fitted background %g", stock, background)
	}
}

func TestConstrainedSharedRatePreservesVoltageGeometry(t *testing.T) {
	records := make([]trialfacts.Record, 20)
	for i := range records {
		outcome := journal.OutcomePass
		if i < 5 {
			outcome = journal.OutcomeFailure
		}
		records[i] = trialfacts.Record{Kind: facts.TrialFact, Profile: slices.Repeat([]int{-25}, 16), Outcome: outcome,
			Class: facts.Class{Regime: machine.R7, Workload: machine.PickWorkload(machine.R7, 0).ID,
				Cores: []int{0, 1, 2, 3, 4, 5, 6, 7}, DurationS: 120}}
	}
	cfg := initialConfig(records)
	cfg.Joints, cfg.CCD, cfg.SharedVoltage = nil, nil, voltageConfig(records)
	for core := range 16 {
		cfg.SharedVoltage.Workload[machine.PickWorkload(machine.R7, 0).ID].Core[core].ThresholdV = 1.135
	}
	syncYcruncher(cfg.SharedVoltage)
	before := cfg.Clone()
	var l voltageLikelihood
	l.cfg, l.obs = cfg, aggregate(records)
	l.rebuild()
	checker, err := modelcheck.NewChecker(cfg, records)
	if err != nil {
		t.Fatal(err)
	}
	if checker.Accepts(l.m) {
		t.Fatal("fixture must initially violate the unchanged group interval")
	}
	all := l.selectObs(func(observation) bool { return true })
	if !l.constrainRate(records, all) || !checker.Accepts(l.m) {
		t.Fatal("a bounded per-time rate must explain the group's failures")
	}
	before.SharedVoltage.Rate = l.cfg.SharedVoltage.Rate
	if diff := cmp.Diff(before, l.cfg); diff != "" {
		t.Fatalf("rate repair changed voltage geometry or background: %s", diff)
	}
	if l.cfg.SharedVoltage.Rate < 1e-5 || l.cfg.SharedVoltage.Rate > .03 {
		t.Fatalf("constrained rate outside physical range: %g", l.cfg.SharedVoltage.Rate)
	}
	for i := range records {
		records[i].Outcome = journal.OutcomePass
	}
	for i := range 20 {
		r := records[i]
		r.Class.DurationS, r.Outcome = 300, journal.OutcomeFailure
		records = append(records, r)
	}
	cfg = initialConfig(records)
	cfg.Joints, cfg.CCD, cfg.SharedVoltage = nil, nil, voltageConfig(records)
	for core := range 16 {
		cfg.SharedVoltage.Workload[machine.PickWorkload(machine.R7, 0).ID].Core[core].ThresholdV = 1.155
	}
	before = cfg.Clone()
	l = voltageLikelihood{}
	l.cfg, l.obs = cfg, aggregate(records)
	l.rebuild()
	all = l.selectObs(func(observation) bool { return true })
	if l.constrainRate(records, all) {
		t.Fatal("incompatible duration groups must not create a passing anchor")
	}
	if diff := cmp.Diff(before, l.cfg); diff != "" {
		t.Fatalf("failed constraint search must restore the best fit: %s", diff)
	}
}
