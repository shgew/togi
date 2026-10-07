package main

import (
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/tools/trialfacts"
)

func TestReportSignalsNamesFittedCounts(t *testing.T) {
	failure := func(regime machine.Regime, signal machine.Signal) trialfacts.Record {
		return trialfacts.Record{Class: facts.Class{Regime: regime}, Outcome: journal.OutcomeFailure, Signal: signal}
	}
	model := sim.DefaultModel()
	cfg := sim.Config{Model: &model}
	fitSignals(&cfg, []trialfacts.Record{failure(machine.R1, machine.UnexpectedExit), failure(machine.R7, machine.Crash), failure(machine.R7, machine.ComputationError)})
	var got strings.Builder
	reportSignals(&got, cfg.Model)
	want := `  signals from 3 failures: computation_error=1 crash=1 unexpected_exit=1
    R1 from 1 failure: unexpected_exit=1
    R2: no failures; pooled mix
    R3: no failures; pooled mix
    R4: no failures; pooled mix
    R5: no failures; pooled mix
    R6: no failures; pooled mix
    R7 from 2 failures: computation_error=1 crash=1
`
	if diff := cmp.Diff(want, got.String()); diff != "" {
		t.Fatalf("report (-want +got):\n%s", diff)
	}
	if model.RegimeSignals != nil {
		t.Fatal("fitSignals changed the caller's model")
	}
	cfg = sim.Config{Model: &model}
	fitSignals(&cfg, []trialfacts.Record{{Class: facts.Class{Regime: machine.R1}, Outcome: journal.OutcomePass}})
	got.Reset()
	reportSignals(&got, cfg.Model)
	if diff := cmp.Diff("  signals: no failures; default weights\n", got.String()); diff != "" {
		t.Fatalf("report without failures (-want +got):\n%s", diff)
	}
}

func TestFitSignalsWithoutSignaledFailuresResetsToDefaultWeights(t *testing.T) {
	model := sim.DefaultModel()
	model.Signals = map[machine.Signal]float64{machine.Crash: 3, machine.ComputationError: 1}
	model.RegimeSignals = map[machine.Regime]map[machine.Signal]float64{machine.R7: {machine.Crash: 3, machine.ComputationError: 1}}
	fitted := sim.DefaultModel()
	fitted.Signals = map[machine.Signal]float64{machine.Crash: 3, machine.ComputationError: 1}
	fitted.RegimeSignals = map[machine.Regime]map[machine.Signal]float64{machine.R7: {machine.Crash: 3, machine.ComputationError: 1}}
	cfg := sim.Config{Model: &model}
	fitSignals(&cfg, []trialfacts.Record{
		{Class: facts.Class{Regime: machine.R7}, Outcome: journal.OutcomePass},
		{Class: facts.Class{Regime: machine.R7}, Outcome: journal.OutcomeFailure},
	})
	want := []any{sim.DefaultModel().Signals, map[machine.Regime]map[machine.Signal]float64(nil)}
	if diff := cmp.Diff(want, []any{cfg.Model.Signals, cfg.Model.RegimeSignals}); diff != "" {
		t.Fatalf("signal mix (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff(fitted, model); diff != "" {
		t.Fatalf("caller's model (-want +got):\n%s", diff)
	}
	var got strings.Builder
	reportSignals(&got, cfg.Model)
	if diff := cmp.Diff("  signals: no failures; default weights\n", got.String()); diff != "" {
		t.Fatalf("report (-want +got):\n%s", diff)
	}
}
