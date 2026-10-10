package sim

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

// fullMachineConfig sets every machine-file field, each to a value that differs from the file's default. Each call
// returns a config that shares nothing with earlier ones.
func fullMachineConfig(t *testing.T) Config {
	t.Helper()
	cfg := voltageConfig(t)
	cfg.Facts = "../facts/extract.jsonl.gz"
	cfg.BIOS = make([]int, 16)
	cfg.BIOS[3] = -7
	cfg.BIOSContext = machine.BIOSContext{BIOSVersion: "A", Board: "fixture", CPUModel: "Zen 5 fixture", Microcode: "0x1", BoostLimitMHz: 5600}
	cfg.Ranking = make([]int, 16)
	for c := range cfg.Ranking {
		cfg.Ranking[c] = 15 - c
	}
	cfg.OldKernel = true
	cfg.CCD = &CCD{LogRate: -9, Slope: 0.1, Effect: [2]float64{0.2, -0.3}}
	cfg.Model = &Model{
		PastLimitRate: 0.015, Growth: 2, NearLimitRate: 1e-7, CrashMCE: 0.25, CoreLocalBank: 0.4, OnsetS: 50, OnsetBoost: 1.5,
		Signals:       map[machine.Signal]float64{machine.Crash: 3, machine.Stall: 1},
		RegimeSignals: map[machine.Regime]map[machine.Signal]float64{machine.R7: {machine.Crash: 2, machine.ComputationError: 1}, machine.R2: {machine.Stall: 1}},
		Reset:         map[machine.ResetKind]float64{machine.ResetWatchdog: 1, machine.ResetPowerLoss: 2},
	}
	cfg.Limits = flat(16, -20, -25)
	idle := -30
	cfg.Limits[0].Idle = &idle
	cfg.Limits[0].Workload = map[string]int{"z": -24, "a": -20}
	cfg.Limits[1].Flat = 0.002
	crashCore := 3
	cfg.Joints = []Joint{
		{Members: map[int]int{1: -40, 10: -35}, Regimes: []machine.Regime{machine.R7, machine.R5}, Rate: 0.005, AfterS: 12.5, Signal: machine.Stall, CrashMCECore: &crashCore},
		{Members: map[int]int{0: -10}, Regimes: []machine.Regime{machine.R7}, Rate: 0.001},
	}
	cfg.Script = map[string]Outcome{
		"0002": {Signal: machine.Stall, AtS: 1.5, Core: 1},
		"0001": {Signal: machine.Crash, AtS: 2, Core: 2, Reset: machine.ResetWatchdog, ThenCrash: true},
	}
	cfg.SharedVoltage.BackgroundRate = 0.00001
	for _, workload := range cfg.SharedVoltage.Workload {
		workload.Core[0].Signals = map[machine.Signal]float64{machine.Crash: 1, machine.Stall: 2}
		workload.Core[0].CountV = 0.004
		workload.Core[0].ThresholdClockVPer100MHz = 0.01
	}
	return cfg
}

func TestCloneSharesNothingWithItsSource(t *testing.T) {
	t.Parallel()
	bios := defaultBIOSContext
	replay, err := NewReplay(bios, []ReplayFact{{Context: bios, Class: journal.TrialClass{Regime: machine.R1, Workload: "work", Cores: []int{0}, DurationS: 60}, Profile: []int{0, 0}, Outcome: journal.OutcomePass, DurationS: 60}})
	if err != nil {
		t.Fatal(err)
	}
	src := fullMachineConfig(t)
	src.Replay = replay
	want := fullMachineConfig(t)
	want.Replay = replay
	clone := src.Clone()
	if diff := cmp.Diff(want, clone, cmp.AllowUnexported(Replay{})); diff != "" {
		t.Fatalf("clone differs from its source (-want +got):\n%s", diff)
	}

	clone.BIOS[3] = 0
	clone.Ranking[0] = -1
	clone.Limits[0].Alone[0] = -1
	*clone.Limits[0].Idle = -1
	clone.Limits[0].Workload["a"] = -1
	clone.Limits[1] = Limits{}
	clone.Model.PastLimitRate = 0.4
	clone.Model.Signals[machine.Crash] = 99
	clone.Model.RegimeSignals[machine.R7][machine.Crash] = 99
	clone.Model.RegimeSignals[machine.R1] = map[machine.Signal]float64{machine.Crash: 1}
	clone.Model.Reset[machine.ResetWatchdog] = 99
	clone.CCD.Slope = 9
	*clone.Replay = Replay{}
	clone.Joints[0].Members[1] = -1
	clone.Joints[0].Regimes[0] = machine.R1
	*clone.Joints[0].CrashMCECore = 9
	clone.Joints[1] = Joint{}
	clone.Script["0001"] = Outcome{}
	clone.SharedVoltage.Rate = 9
	for id, workload := range clone.SharedVoltage.Workload {
		workload.Core[0].BaseV = 9
		workload.Core[0].Signals[machine.Crash] = 99
		workload.Core[1] = VoltageCore{}
		clone.SharedVoltage.Workload[id] = workload
	}
	delete(clone.SharedVoltage.Workload, machine.PickWorkload(machine.R7, 0).ID)

	if diff := cmp.Diff(want, src, cmp.AllowUnexported(Replay{})); diff != "" {
		t.Fatalf("changing the clone changed its source (-want +got):\n%s", diff)
	}
	if src.Replay.context != bios {
		t.Fatal("changing the clone's replay changed its source")
	}
}
