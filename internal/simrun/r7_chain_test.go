package simrun

import (
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
)

func TestR7SimulationRunsMeasuredFullFirstChains(t *testing.T) {
	cfg := huntConfig(16)
	for i := range cfg.Limits {
		cfg.Limits[i].Alone = [5]int{-50, -50, -50, -50, -50}
		cfg.Limits[i].Together = [7]int{-50, -50, -50, -50, -50, -50, -50}
	}
	voltage := &sim.SharedVoltage{IdleV: .8, MarginV: .002, Rate: .0008, PowerLimitW: 500, ThermalLimitW: 500, Workload: map[string]sim.VoltageWorkload{}}
	for _, w := range machine.Workloads(machine.R7) {
		cores := make([]sim.VoltageCore, 16)
		for id := range cores {
			cores[id] = sim.VoltageCore{BaseV: 1.2+float64(id%8)*.005, ThresholdV: .5}
		}
		voltage.Workload[w.ID] = sim.VoltageWorkload{ReferenceMHz: 5000, FullMHz: [2]float64{5000, 5000}, WattsPerCore: 10, Core: cores}
	}
	cfg.SharedVoltage = voltage
	_, events, _ := runHunt(t, cfg, nil, func(in *Input) {
		in.Config.CandidateSoloLimits = make([]int, 16)
		for id := range in.Config.CandidateSoloLimits { in.Config.CandidateSoloLimits[id] = -50 }
		in.Config.Checking.Lap = []machine.Regime{machine.R7}
		in.Until = func(e journal.Event) bool {
			p, ok := e.Data.(*journal.CheckingLap)
			return ok && p.Event == journal.LapEnd && p.Passed
		}
	})
	var got [][]int
	chains, lastEnd := 0, 0
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.TrialIntent:
			if p.Regime == machine.R7 && p.Phase == journal.PhaseChecking {
				if p.RecordOnly { t.Fatal("simulator emitted record-only partial") }
				got = append(got, p.Cores)
			}
		case *journal.TrialEnd:
			lastEnd = e.Seq
		case *journal.CheckingChain:
			chains++
			if !slices.Equal(p.SourceSeqs, []int{lastEnd}) || !slices.Contains(e.Cause, lastEnd) {
				t.Fatalf("chain omitted its predecessor measurement: %+v cause %v, last end %d", p, e.Cause, lastEnd)
			}
			if len(p.Cores) != 0 && len(p.Cores) < 2 { t.Fatal("chain loaded fewer than two cores") }
		case *journal.HuntStart:
			t.Fatal("passing measured chains started a hunt")
		}
	}
	var want [][]int
	for _, base := range []int{0, 8} {
		for loaded := 8; loaded >= 2; loaded-- {
			cores := make([]int, loaded)
			for i := range cores { cores[i] = base+i }
			for range 4 { want = append(want, cores) }
		}
	}
	all := make([]int, 16)
	for id := range all { all[id] = id }
	for range 4 { want = append(want, all) }
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("measured checking sequence (-want +got):\n%s", diff)
	}
	if chains != 14 { t.Fatalf("chain derivations = %d, want 14 including terminal derivations", chains) }
}
