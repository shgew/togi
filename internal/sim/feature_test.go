package sim

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func runSpec(t *testing.T, m *Machine, id string, regime machine.Regime, workload machine.Workload, cores []int, duration time.Duration, report machine.Reporter) (machine.Result, error) {
	t.Helper()
	run, err := m.Seams().Trials.Start(context.Background(), machine.TrialSpec{ID: id, Regime: regime, Workload: workload, Condition: machine.Resident, Cores: cores, Duration: duration})
	if err != nil {
		t.Fatalf("start %s: %v", id, err)
	}
	return run.Wait(context.Background(), report)
}

func TestModelHazards(t *testing.T) {
	work := machine.PickWorkload(machine.R1, 0)
	t.Run("onset inversion", func(t *testing.T) {
		base := sharp(machine.ComputationError)
		base.PastEdgeRate = 0.1
		base.OnsetS = 10
		base.OnsetBoost = 0
		boosted := *base
		boosted.OnsetBoost = 9
		for _, tc := range []struct {
			name  string
			model *Model
		}{{"base", base}, {"boost", &boosted}} {
			t.Run(tc.name, func(t *testing.T) {
				m := newMachine(t, Config{Seed: 42, Cores: 2, BIOS: []int{-11, 0}, Edges: flat(2, -10, -10), Model: tc.model})
				res, err := runSpec(t, m, "0010", machine.R1, work, []int{0}, time.Minute, nil)
				if err != nil || res.Signal != machine.ComputationError {
					t.Fatalf("hazard: %+v %v", res, err)
				}
				rate := base.PastEdgeRate
				x := m.trialRNG("trial", machine.TrialSpec{Regime: machine.R1, Condition: machine.Resident, Index: 0}, 0).ExpFloat64()
				want := x / rate
				if tc.model.OnsetBoost > 0 {
					want = x / (rate * (1 + tc.model.OnsetBoost))
					if want > tc.model.OnsetS {
						want = tc.model.OnsetS + (x-tc.model.OnsetS*rate*(1+tc.model.OnsetBoost))/rate
					}
				}
				if diff := cmp.Diff(time.Duration(want*float64(time.Second)), res.Ran); diff != "" {
					t.Fatalf("failure time (-want +got): %s", diff)
				}
			})
		}
	})
	t.Run("joint after threshold", func(t *testing.T) {
		m := newMachine(t, Config{Cores: 2, BIOS: []int{-20, -20}, Edges: flat(2, -30, -30), Model: sharp(machine.Crash), Joints: []Joint{{Members: map[int]int{0: -20, 1: -20}, Regimes: []machine.Regime{machine.R7}, Rate: 1e6, AfterS: 30}}})
		if res, err := runSpec(t, m, "0001", machine.R1, work, []int{0}, time.Minute, nil); err != nil || res.Signal != "" {
			t.Fatalf("wrong regime: %+v %v", res, err)
		}
		if res, err := runSpec(t, m, "0002", machine.R7, work, []int{0}, 20*time.Second, nil); err != nil || res.Signal != "" {
			t.Fatalf("joint before threshold: %+v %v", res, err)
		}
		_, err := runSpec(t, m, "0003", machine.R7, work, []int{0}, time.Minute, nil)
		if !errors.Is(err, machine.ErrCrashed) || m.Monotonic() < 110*time.Second {
			t.Fatalf("joint crash %v at %s", err, m.Monotonic())
		}
	})
	t.Run("joint attribution follows loaded order", func(t *testing.T) {
		m := newMachine(t, Config{
			Cores: 2, BIOS: []int{-20, -20}, Edges: flat(2, -30, -30), Model: sharp(machine.Crash),
			Joints: []Joint{{Members: map[int]int{0: -20, 1: -20}, Rate: 1e6, Signal: machine.ComputationError}},
		})
		res, err := runSpec(t, m, "0001", machine.R7, work, []int{1, 0}, time.Second, nil)
		if err != nil || res.Signal != machine.ComputationError || res.Core != 1 {
			t.Fatalf("attributed joint: %+v %v", res, err)
		}
	})
	t.Run("joint default crash overrides model signal", func(t *testing.T) {
		model := sharp(machine.ComputationError)
		model.CrashMCE = 0
		m := newMachine(t, Config{Cores: 2, BIOS: []int{-20, 0}, Edges: flat(2, -30, -30), Model: model, Joints: []Joint{{Members: map[int]int{0: -20}, Rate: 1e6}}})
		if _, err := runSpec(t, m, "0001", machine.R1, work, []int{0}, time.Second, nil); !errors.Is(err, machine.ErrCrashed) {
			t.Fatalf("default joint signal: %v", err)
		}
	})
	t.Run("idle and flat", func(t *testing.T) {
		idle := -10
		edges := flat(2, -30, -30)
		edges[1].Idle = &idle
		m := newMachine(t, Config{Cores: 2, BIOS: []int{0, -11}, Edges: edges, Model: sharp(machine.ComputationError)})
		_, err := runSpec(t, m, "0001", machine.R1, work, []int{0}, time.Second, nil)
		if !errors.Is(err, machine.ErrCrashed) {
			t.Fatalf("idle core did not crash: %v", err)
		}
		m.Reboot()
		boot, _ := m.Seams().Host.BootID()
		mces, err := m.Seams().Kernel.MCEs(boot, 0)
		if err != nil || len(mces) != 0 {
			t.Fatalf("idle crash carried MCE %+v %v", mces, err)
		}
		edges[1].Idle = nil
		edges[1].Flat = 1e6
		m = newMachine(t, Config{Cores: 2, BIOS: []int{0, -1}, Edges: edges, Model: sharp(machine.ComputationError)})
		_, err = runSpec(t, m, "0001", machine.R1, work, []int{0}, time.Second, nil)
		if !errors.Is(err, machine.ErrCrashed) {
			t.Fatalf("flat idle core did not crash: %v", err)
		}
	})
	t.Run("workload and register edge", func(t *testing.T) {
		edges := flat(2, -20, -10)
		edges[0].Workload = map[string]int{"special": -5}
		m := newMachine(t, Config{Cores: 2, BIOS: []int{-15, 0}, Edges: edges, Model: sharp(machine.ComputationError)})
		if res, err := runSpec(t, m, "0001", machine.R1, work, []int{0}, time.Second, nil); err != nil || res.Signal != "" {
			t.Fatalf("isolated register edge: %+v %v", res, err)
		}
		if res, err := runSpec(t, m, "0002", machine.R1, machine.Workload{ID: "special"}, []int{0}, time.Second, nil); err != nil || res.Signal != machine.ComputationError {
			t.Fatalf("workload edge: %+v %v", res, err)
		}
		if err := m.Seams().SMU.SetOffset(1, -1); err != nil {
			t.Fatal(err)
		}
		if res, err := runSpec(t, m, "0003", machine.R1, work, []int{0}, time.Second, nil); err != nil || res.Signal != machine.ComputationError {
			t.Fatalf("resident register edge: %+v %v", res, err)
		}
	})
	t.Run("tiny rate cannot overflow into a crash", func(t *testing.T) {
		edges := flat(2, -10, -10)
		edges[0].Flat = 1e-300
		m := newMachine(t, Config{Cores: 2, BIOS: []int{-1, 0}, Edges: edges})
		if res, err := runSpec(t, m, "0001", machine.R1, work, []int{0}, time.Minute, nil); err != nil || res.Signal != "" {
			t.Fatalf("tiny hazard: %+v %v", res, err)
		}
	})
}

func TestScriptsAndClocks(t *testing.T) {
	m := newMachine(t, Config{Cores: 2, Edges: flat(2, -50, -50), Script: map[string]Outcome{
		"0001": {Signal: machine.CorrectedMCE, AtS: 7, Core: 0},
		"0002": {Signal: machine.ComputationError, AtS: 3, Core: 1, ThenCrash: true, Reset: machine.ResetThermalTrip},
	}})
	boot, _ := m.Seams().Host.BootID()
	m.JumpWall(-time.Hour)
	if res, err := runSpec(t, m, "0001", machine.R1, machine.PickWorkload(machine.R1, 0), []int{0}, 10*time.Second, nil); err != nil || res.Signal != "" {
		t.Fatalf("script MCE: %+v %v", res, err)
	}
	mces, err := m.Seams().Kernel.MCEs(boot, 7*time.Second)
	if err != nil || len(mces) != 1 || mces[0].Monotonic != 7*time.Second || !mces[0].Time.Equal(epoch.Add(-time.Hour+7*time.Second)) {
		t.Fatalf("MCE time: %+v %v", mces, err)
	}
	older, _ := m.Seams().Kernel.MCEs(boot, 7*time.Second+1)
	if diff := cmp.Diff(0, len(older)); diff != "" {
		t.Fatal(diff)
	}
	if err := m.Sleep(context.Background(), 5*time.Second); err != nil || m.Monotonic() != 15*time.Second || !m.Now().Equal(epoch.Add(-time.Hour+15*time.Second)) {
		t.Fatalf("sleep: %v wall %s mono %s", err, m.Now(), m.Monotonic())
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if err := m.Sleep(ctx, time.Second); !errors.Is(err, context.Canceled) || m.Monotonic() != 15*time.Second {
		t.Fatalf("cancel sleep: %v", err)
	}
	report := &signalRecorder{}
	_, err = runSpec(t, m, "0002", machine.R1, machine.PickWorkload(machine.R1, 0), []int{1}, time.Minute, report)
	if !errors.Is(err, machine.ErrCrashed) || !cmp.Equal(report.signals, []machine.Signal{machine.ComputationError}) {
		t.Fatalf("script signal %v: %v", report.signals, err)
	}
	m.Reboot()
	boot, _ = m.Seams().Host.BootID()
	reason, err := m.Seams().Kernel.ResetReason(boot)
	if err != nil || reason.Kind != machine.ResetThermalTrip || !strings.Contains(reason.Raw, "thermal pin BP_THERMTRIP_L was tripped") {
		t.Fatalf("reset reason %+v %v", reason, err)
	}
	if m.Monotonic() != 0 {
		t.Fatalf("reboot monotonic %s", m.Monotonic())
	}
}

func TestScriptThenCrashReportsOnlyBackendSignals(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name   string
		signal machine.Signal
		want   []machine.Signal
	}{
		{"crash", machine.Crash, nil},
		{"computation error", machine.ComputationError, []machine.Signal{machine.ComputationError}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			m := newMachine(t, Config{Cores: 2, Script: map[string]Outcome{"0001": {Signal: tc.signal, Core: 0, ThenCrash: true}}})
			report := &signalRecorder{}
			_, err := runSpec(t, m, "0001", machine.R7, machine.PickWorkload(machine.R7, 0), []int{0}, time.Second, report)
			if !errors.Is(err, machine.ErrCrashed) {
				t.Fatalf("scripted crash: %v", err)
			}
			if diff := cmp.Diff(tc.want, report.signals); diff != "" {
				t.Fatalf("reported signals (-want +got): %s", diff)
			}
			if _, err := m.Seams().Host.BootID(); !errors.Is(err, machine.ErrCrashed) {
				t.Fatalf("host after crash: %v", err)
			}
		})
	}
}

type signalRecorder struct{ signals []machine.Signal }

func (*signalRecorder) Progress(string)       {}
func (*signalRecorder) Sample(machine.Sample) {}
func (r *signalRecorder) Signal(_ int, signal machine.Signal, _ string) {
	r.signals = append(r.signals, signal)
}

func TestFaultsAndRanking(t *testing.T) {
	if ranking, err := newMachine(t, Config{Cores: 2}).Seams().Host.Ranking(); err == nil || ranking != nil {
		t.Fatalf("unavailable ranking: %v %v", ranking, err)
	}
	m := newMachine(t, Config{Cores: 2, Ranking: []int{11, 22}, OldKernel: true})
	ranking, err := m.Seams().Host.Ranking()
	if err != nil || !cmp.Equal(ranking, []int{11, 22}) {
		t.Fatalf("ranking %v %v", ranking, err)
	}
	ranking[0] = 99
	if got, _ := m.Seams().Host.Ranking(); got[0] != 11 {
		t.Fatalf("ranking aliased: %v", got)
	}
	m.FailWriteAt(2)
	if err := m.Seams().SMU.SetOffset(0, -1); err != nil {
		t.Fatal(err)
	}
	if err := m.Seams().SMU.SetOffset(1, -2); err == nil {
		t.Fatal("second write succeeded")
	}
	if o, _ := m.Seams().SMU.Offset(1); o != 0 {
		t.Fatalf("failed write applied %d", o)
	}
	m.CrashAtWrite(1)
	if err := m.Seams().SMU.SetOffset(1, -2); !errors.Is(err, machine.ErrCrashed) {
		t.Fatalf("crash at write: %v", err)
	}
	m.Reboot()
	m.MissBackend(machine.Mprime)
	_, err = m.Seams().Trials.Start(context.Background(), machine.TrialSpec{Workload: machine.Workload{Backend: machine.Mprime}})
	if !errors.Is(err, machine.ErrBackendMissing) {
		t.Fatalf("missing backend: %v", err)
	}
	m.PowerLoss()
	m.Reboot()
	boot, _ := m.Seams().Host.BootID()
	reason, _ := m.Seams().Kernel.ResetReason(boot)
	if reason.Kind != "" || reason.Supported || reason.Raw != "" {
		t.Fatalf("old kernel power loss: %+v", reason)
	}
	m.HoldPowerButton()
	m.Reboot()
	boot, _ = m.Seams().Host.BootID()
	reason, _ = m.Seams().Kernel.ResetReason(boot)
	if reason.Kind != "" || reason.Supported || reason.Raw != "" {
		t.Fatalf("old kernel power button: %+v", reason)
	}
}

func TestOldKernelDoesNotReportResetKind(t *testing.T) {
	t.Parallel()
	m := newMachine(t, Config{Cores: 2, OldKernel: true})
	m.NextReset(machine.ResetThermalTrip)
	m.Crash()
	m.Reboot()
	boot, err := m.Seams().Host.BootID()
	if err != nil {
		t.Fatal(err)
	}
	reason, err := m.Seams().Kernel.ResetReason(boot)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(machine.ResetReason{}, reason); diff != "" {
		t.Fatalf("unsupported reset reason (-want +got): %s", diff)
	}
}

func TestResetReasonKinds(t *testing.T) {
	for _, tc := range []struct {
		kind machine.ResetKind
		line string
	}{
		{machine.ResetWatchdog, "hardware watchdog timer expired"},
		{machine.ResetSyncFlood, "an uncorrected error caused a data fabric sync flood event"},
		{machine.ResetCPUShutdown, "internal CPU shutdown event occurred"},
		{machine.ResetPowerButton, "power button was pressed for 4 seconds"},
		{machine.ResetThermalTrip, "thermal pin BP_THERMTRIP_L was tripped"},
		{machine.ResetUnknown, "unrecognized reset reason"},
		{machine.ResetPowerLoss, ""},
	} {
		t.Run(string(tc.kind), func(t *testing.T) {
			m := newMachine(t, Config{Cores: 2})
			m.NextReset(tc.kind)
			m.Crash()
			m.Reboot()
			boot, _ := m.Seams().Host.BootID()
			reason, err := m.Seams().Kernel.ResetReason(boot)
			wantKind := tc.kind
			if tc.kind == machine.ResetPowerLoss {
				wantKind = ""
			}
			if err != nil || reason.Kind != wantKind || !reason.Supported {
				t.Fatalf("reason %+v %v", reason, err)
			}
			if tc.line != "" && !strings.Contains(reason.Raw, tc.line) || tc.line == "" && reason.Raw != "" {
				t.Fatalf("kernel line %q, want %q", reason.Raw, tc.line)
			}
		})
	}
}

func TestWeightedCrashReset(t *testing.T) {
	model := sharp(machine.Crash)
	model.Reset = map[machine.ResetKind]float64{machine.ResetSyncFlood: 1}
	model.CrashMCE = 0
	m := newMachine(t, Config{Cores: 2, BIOS: []int{-11, 0}, Edges: flat(2, -10, -10), Model: model})
	if _, err := runSpec(t, m, "0001", machine.R1, machine.PickWorkload(machine.R1, 0), []int{0}, time.Second, nil); !errors.Is(err, machine.ErrCrashed) {
		t.Fatalf("weighted crash: %v", err)
	}
	m.Reboot()
	boot, _ := m.Seams().Host.BootID()
	if reason, err := m.Seams().Kernel.ResetReason(boot); err != nil || reason.Kind != machine.ResetSyncFlood {
		t.Fatalf("weighted reset reason %+v %v", reason, err)
	}
}

func TestLoadMachine(t *testing.T) {
	path := filepath.Join(t.TempDir(), "machine.toml")
	content := `cores = 2
bios = [0, -1]
ranking = [9, 7]
old_kernel = true
[model]
past_edge_rate = 0.7
onset_boost = 2
[model.signals]
crash = 1
[model.reset]
thermal_trip = 1
[[core]]
id = 0
isolated = [-30, -30, -30, -30, -30]
resident = [-29, -29, -29, -29, -29, -29, -29]
idle = -45
workload = { "special" = -27 }
flat = 0.1
[[core]]
id = 1
isolated = [-30, -30, -30, -30, -30]
resident = [-29, -29, -29, -29, -29, -29, -29]
[[joint]]
members = { "0" = -20, "1" = -20 }
regimes = ["R7"]
rate = 0.05
after_s = 30
signal = "crash"
[[script]]
trial = "0042"
signal = "crash"
at_s = 3
core = 1
reset = "watchdog"
then_crash = true
`
	if err := os.WriteFile(path, []byte(content), 0600); err != nil {
		t.Fatal(err)
	}
	cfg, err := LoadMachine(path)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]int{9, 7}, cfg.Ranking); diff != "" {
		t.Fatal(diff)
	}
	if cfg.Model.PastEdgeRate != .7 || cfg.Model.OnsetS != 100 || cfg.Model.Reset[machine.ResetThermalTrip] != 1 || cfg.Edges[0].Workload["special"] != -27 || cfg.Joints[0].AfterS != 30 || !cfg.Script["0042"].ThenCrash {
		t.Fatalf("incomplete config: %+v", cfg)
	}
	if err := os.WriteFile(path, []byte(content+"unknown = 1\n"), 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadMachine(path); err == nil || !strings.Contains(err.Error(), path) || !strings.Contains(err.Error(), "unknown") {
		t.Fatalf("unknown key error: %v", err)
	}
}

func TestNewRejectsInvalidScriptCore(t *testing.T) {
	t.Parallel()
	for _, core := range []int{-1, 2} {
		_, err := New(Config{Cores: 2, Script: map[string]Outcome{"0001": {Signal: machine.Crash, Core: core}}})
		want := fmt.Sprintf("new simulator: script trial 0001 core %d outside [0, 2)", core)
		if err == nil || err.Error() != want {
			t.Fatalf("core %d: got %v, want %s", core, err, want)
		}
	}
}

func TestNewRejectsInvalidModelWeights(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		signals map[machine.Signal]float64
		reset   map[machine.ResetKind]float64
		want    string
	}{
		{"signal unknown sorted", map[machine.Signal]float64{"z": 1, "a": 1}, nil, `new simulator: signal "a" is not supported`},
		{"signal negative sorted", map[machine.Signal]float64{machine.Stall: -1, machine.ComputationError: -2}, nil, `new simulator: signal "computation_error" weight -2 is negative`},
		{"reset unknown sorted", nil, map[machine.ResetKind]float64{"z": 1, "a": 1}, `new simulator: reset "a" is not supported`},
		{"reset negative sorted", nil, map[machine.ResetKind]float64{machine.ResetWatchdog: -1, machine.ResetThermalTrip: -2}, `new simulator: reset "thermal_trip" weight -2 is negative`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			model := DefaultModel()
			if tc.signals != nil {
				model.Signals = tc.signals
			}
			if tc.reset != nil {
				model.Reset = tc.reset
			}
			_, err := New(Config{Cores: 2, Model: &model})
			if err == nil || err.Error() != tc.want {
				t.Fatalf("got %v, want %s", err, tc.want)
			}
		})
	}
}

func TestLoadMachineRejectsInvalidCoreCountBeforeEdges(t *testing.T) {
	t.Parallel()
	path := filepath.Join(t.TempDir(), "machine.toml")
	if err := os.WriteFile(path, []byte("cores = -2\n[[core]]\nid = 0\n"), 0600); err != nil {
		t.Fatal(err)
	}
	_, err := LoadMachine(path)
	want := fmt.Sprintf("load simulator machine %s: new simulator: -2 cores: must be even and at least 2", path)
	if err == nil || err.Error() != want {
		t.Fatalf("got %v, want %s", err, want)
	}
}

func TestLoadMachineBIOSContext(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		content string
		want    machine.BIOSContext
	}{
		{"unset", "cores = 2\n", machine.BIOSContext{}},
		{"present", "cores = 2\n[bios_context]\nbios_version = \"B.2\"\nboard = \"X670\"\ncpu_model = \"Zen 5\"\nmicrocode = \"0x123\"\nboost_limit_mhz = 5900\n", machine.BIOSContext{BIOSVersion: "B.2", Board: "X670", CPUModel: "Zen 5", Microcode: "0x123", BoostLimitMHz: 5900}},
		{"partial", "cores = 2\n[bios_context]\nbios_version = \"SIM.2\"\n", machine.BIOSContext{BIOSVersion: "SIM.2", Board: defaultBIOSContext.Board, CPUModel: defaultBIOSContext.CPUModel, Microcode: defaultBIOSContext.Microcode, BoostLimitMHz: defaultBIOSContext.BoostLimitMHz}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			path := filepath.Join(t.TempDir(), "machine.toml")
			if err := os.WriteFile(path, []byte(tc.content), 0600); err != nil {
				t.Fatal(err)
			}
			cfg, err := LoadMachine(path)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.want, cfg.BIOSContext); diff != "" {
				t.Fatalf("BIOS context (-want +got): %s", diff)
			}
			m := newMachine(t, cfg)
			context, err := m.Seams().Host.BIOSContext()
			if err != nil {
				t.Fatal(err)
			}
			expected := tc.want
			if expected == (machine.BIOSContext{}) {
				expected = defaultBIOSContext
			}
			if diff := cmp.Diff(expected, context); diff != "" {
				t.Fatalf("host BIOS context (-want +got): %s", diff)
			}
		})
	}
}
