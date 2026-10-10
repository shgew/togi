package main

import (
	"fmt"
	"io"
	"maps"
	"slices"
	"strings"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/tools/trialfacts"
)

// fitSignals sets the failure signal mix to the counts of the records' failure signals: pooled over every regime
// in model.signals, which regimes without failures draw from, and per regime with failures in
// model.regime_signals. Signal weights are relative, so counts are their maximum-likelihood estimate. An
// uncorrected MCE counts as a crash, which the simulator draws as a crash leaving an MCE with probability
// crash_mce. Failures without a recorded signal are skipped; without any, the mix is the simulator's default
// weights and no regime_signals, whatever the starting model held.
func fitSignals(cfg *sim.Config, records []trialfacts.Record) {
	pooled := map[machine.Signal]float64{}
	regimes := map[machine.Regime]map[machine.Signal]float64{}
	for _, r := range records {
		if r.Outcome != journal.OutcomeFailure || r.Signal == "" {
			continue
		}
		signal := r.Signal
		if signal == machine.UncorrectedMCE {
			signal = machine.Crash
		}
		pooled[signal]++
		if regimes[r.Class.Regime] == nil {
			regimes[r.Class.Regime] = map[machine.Signal]float64{}
		}
		regimes[r.Class.Regime][signal]++
	}
	model := *cfg.Model
	if len(pooled) == 0 {
		model.Signals, model.RegimeSignals = sim.DefaultModel().Signals, nil
	} else {
		model.Signals, model.RegimeSignals = pooled, regimes
	}
	cfg.Model = &model
}

// reportSignals prints the fitted signal mix as failure counts, pooled and per regime.
func reportSignals(w io.Writer, model *sim.Model) {
	if model.RegimeSignals == nil {
		fmt.Fprintln(w, "  signals: no failures; default weights")
		return
	}
	fmt.Fprintf(w, "  signals from %s: %s\n", failureCount(model.Signals), signalCounts(model.Signals))
	for _, regime := range machine.Regimes {
		if counts, ok := model.RegimeSignals[regime]; ok {
			fmt.Fprintf(w, "    %s from %s: %s\n", regime, failureCount(counts), signalCounts(counts))
		} else {
			fmt.Fprintf(w, "    %s: no failures; pooled mix\n", regime)
		}
	}
}

// signalsNote says how many failures a machine's signal weights were counted from.
func signalsNote(model *sim.Model) string {
	if model.RegimeSignals == nil {
		return "Signal mix: no failures; default weights."
	}
	return fmt.Sprintf("Signal mix: counted from %s.", failureCount(model.Signals))
}

func failureCount(counts map[machine.Signal]float64) string {
	var total float64
	for _, n := range counts {
		total += n
	}
	if total == 1 {
		return "1 failure"
	}
	return fmt.Sprintf("%g failures", total)
}

func signalCounts(counts map[machine.Signal]float64) string {
	var parts []string
	for _, signal := range slices.Sorted(maps.Keys(counts)) {
		parts = append(parts, fmt.Sprintf("%s=%g", signal, counts[signal]))
	}
	return strings.Join(parts, " ")
}
