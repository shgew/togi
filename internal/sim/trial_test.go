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

func TestTrialRNGStableConditionDomains(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		condition machine.Condition
		domain    string
	}{
		{machine.Alone, "isolated"},
		{machine.Together, "resident"},
		{machine.Parked, "masked"},
	} {
		t.Run(string(tc.condition), func(t *testing.T) {
			t.Parallel()
			model := sharp(machine.ComputationError)
			model.PastLimitRate = 0.1
			m := newMachine(t, Config{Seed: 42, Cores: 2, BIOS: []int{-11, 0}, Limits: flat(2, -10, -10), Model: model})
			spec := machine.TrialSpec{
				ID: "0008", Index: 7, Regime: machine.R1, Workload: machine.PickWorkload(machine.R1, 7),
				Condition: tc.condition, Cores: []int{0}, CPUs: []int{0}, Duration: time.Minute,
			}
			for _, purpose := range []string{"trial", "signal", "tctl", "ccd-0", "joint-0"} {
				wantRNG := m.rng(purpose, 0, spec.Regime, tc.domain, -11, spec.Index)
				gotRNG := m.trialRNG(purpose, spec, 0)
				want := [3]uint64{wantRNG.Uint64(), wantRNG.Uint64(), wantRNG.Uint64()}
				got := [3]uint64{gotRNG.Uint64(), gotRNG.Uint64(), gotRNG.Uint64()}
				if diff := cmp.Diff(want, got); diff != "" {
					t.Fatalf("%s draw stream (-want +got):\n%s", purpose, diff)
				}
			}
			hazard := m.rng("trial", 0, spec.Regime, tc.domain, -11, spec.Index).ExpFloat64()
			wantDuration := min(spec.Duration, time.Duration(hazard/model.PastLimitRate*float64(time.Second)))
			run, err := m.Seams().Trials.Start(context.Background(), spec)
			if err != nil {
				t.Fatal(err)
			}
			result, err := run.Wait(context.Background(), nil)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(wantDuration, result.Ran); diff != "" {
				t.Fatalf("trial duration (-want +got):\n%s", diff)
			}
		})
	}
}

func TestModelHazards(t *testing.T) {
	work := machine.PickWorkload(machine.R1, 0)
	t.Run("onset inversion", func(t *testing.T) {
		base := sharp(machine.ComputationError)
		base.PastLimitRate = 0.1
		base.OnsetS = 10
		base.OnsetBoost = 0
		boosted := *base
		boosted.OnsetBoost = 9
		for _, tc := range []struct {
			name  string
			model *Model
		}{{"base", base}, {"boost", &boosted}} {
			t.Run(tc.name, func(t *testing.T) {
				m := newMachine(t, Config{Seed: 42, Cores: 2, BIOS: []int{-11, 0}, Limits: flat(2, -10, -10), Model: tc.model})
				res, err := runSpec(t, m, "0010", machine.R1, work, []int{0}, time.Minute, nil)
				if err != nil || res.Signal != machine.ComputationError {
					t.Fatalf("hazard: %+v %v", res, err)
				}
				rate := base.PastLimitRate
				x := m.trialRNG("trial", machine.TrialSpec{Regime: machine.R1, Condition: machine.Together, Cores: []int{0}, Index: 0}, 0).ExpFloat64()
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
		m := newMachine(t, Config{Cores: 2, BIOS: []int{-20, -20}, Limits: flat(2, -30, -30), Model: sharp(machine.Crash), Joints: []Joint{{Members: map[int]int{0: -20, 1: -20}, Regimes: []machine.Regime{machine.R7}, Rate: 1e6, AfterS: 30}}})
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
			Cores: 2, BIOS: []int{-20, -20}, Limits: flat(2, -30, -30), Model: sharp(machine.Crash),
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
		m := newMachine(t, Config{Cores: 2, BIOS: []int{-20, 0}, Limits: flat(2, -30, -30), Model: model, Joints: []Joint{{Members: map[int]int{0: -20}, Rate: 1e6}}})
		if _, err := runSpec(t, m, "0001", machine.R1, work, []int{0}, time.Second, nil); !errors.Is(err, machine.ErrCrashed) {
			t.Fatalf("default joint signal: %v", err)
		}
	})
	t.Run("unloaded joint crashes as an idle member", func(t *testing.T) {
		model := sharp(machine.ComputationError)
		model.CrashMCE = 1
		m := newMachine(t, Config{Cores: 4, BIOS: []int{0, -20, -20, 0}, Limits: flat(4, -30, -30), Model: model, Joints: []Joint{{Members: map[int]int{1: -20, 2: -20}, Rate: 1e6}}})
		if _, err := runSpec(t, m, "0001", machine.R1, work, []int{0}, time.Second, nil); !errors.Is(err, machine.ErrCrashed) {
			t.Fatalf("unloaded joint: %v", err)
		}
		m.Reboot()
		boot, _ := m.Seams().Host.BootID()
		mces, err := m.Seams().Kernel.MCEs(boot, 0)
		if err != nil || len(mces) != 0 {
			t.Fatalf("unloaded joint named cores through MCEs %+v %v", mces, err)
		}
	})
	t.Run("idle and flat", func(t *testing.T) {
		idle := -10
		limits := flat(2, -30, -30)
		limits[1].Idle = &idle
		m := newMachine(t, Config{Cores: 2, BIOS: []int{0, -11}, Limits: limits, Model: sharp(machine.ComputationError)})
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
		limits[1].Idle = nil
		limits[1].Flat = 1e6
		m = newMachine(t, Config{Cores: 2, BIOS: []int{0, -1}, Limits: limits, Model: sharp(machine.ComputationError)})
		_, err = runSpec(t, m, "0001", machine.R1, work, []int{0}, time.Second, nil)
		if !errors.Is(err, machine.ErrCrashed) {
			t.Fatalf("flat idle core did not crash: %v", err)
		}
	})
	t.Run("workload and register limit", func(t *testing.T) {
		limits := flat(2, -20, -10)
		limits[0].Workload = map[string]int{"special": -5}
		m := newMachine(t, Config{Cores: 2, BIOS: []int{-15, 0}, Limits: limits, Model: sharp(machine.ComputationError)})
		if res, err := runSpec(t, m, "0001", machine.R1, work, []int{0}, time.Second, nil); err != nil || res.Signal != "" {
			t.Fatalf("alone register limit: %+v %v", res, err)
		}
		if res, err := runSpec(t, m, "0002", machine.R1, machine.Workload{ID: "special"}, []int{0}, time.Second, nil); err != nil || res.Signal != machine.ComputationError {
			t.Fatalf("workload limit: %+v %v", res, err)
		}
		if err := m.Seams().SMU.SetOffset(1, -1); err != nil {
			t.Fatal(err)
		}
		if res, err := runSpec(t, m, "0003", machine.R1, work, []int{0}, time.Second, nil); err != nil || res.Signal != machine.ComputationError {
			t.Fatalf("together register limit: %+v %v", res, err)
		}
	})
	t.Run("tiny rate cannot overflow into a crash", func(t *testing.T) {
		limits := flat(2, -10, -10)
		limits[0].Flat = 1e-300
		m := newMachine(t, Config{Cores: 2, BIOS: []int{-1, 0}, Limits: limits})
		if res, err := runSpec(t, m, "0001", machine.R1, work, []int{0}, time.Minute, nil); err != nil || res.Signal != "" {
			t.Fatalf("tiny hazard: %+v %v", res, err)
		}
	})
}

func TestFailureDrawAfterOnset(t *testing.T) {
	model := sharp(machine.ComputationError)
	model.PastLimitRate, model.Growth, model.OnsetS, model.OnsetBoost = 0.001, 1, 1, 2
	m := newMachine(t, Config{Seed: 42, Cores: 2, BIOS: []int{-11, 0}, Limits: flat(2, -10, -10), Model: model})
	spec := machine.TrialSpec{Regime: machine.R1, Condition: machine.Together, Cores: []int{0}}
	x := m.trialRNG("trial", spec, 0).ExpFloat64()
	want := time.Duration((1 + (x-0.003)/0.001) * float64(time.Second))
	if want <= time.Second {
		t.Fatal("seed does not reach post-onset exposure")
	}
	res, err := runSpec(t, m, "0001", machine.R1, machine.PickWorkload(machine.R1, 0), []int{0}, 24*time.Hour, nil)
	if err != nil || res.Signal != machine.ComputationError {
		t.Fatalf("result = %+v, %v", res, err)
	}
	if diff := cmp.Diff(want, res.Ran); diff != "" {
		t.Fatal(diff)
	}
}

func TestUnloadedDrawFollowsLoadedSet(t *testing.T) {
	model := DefaultModel()
	model.NearLimitRate = 0
	limits := flat(4, -30, -30)
	limits[2].Flat = 1.0 / 60
	cfg := Config{Cores: 4, BIOS: []int{0, 0, -5, 0}, Limits: limits, Model: &model}
	crashAt := func(cores []int) time.Duration {
		t.Helper()
		m := newMachine(t, cfg)
		if _, err := runSpec(t, m, "0001", machine.R1, machine.PickWorkload(machine.R1, 0), cores, 24*time.Hour, nil); !errors.Is(err, machine.ErrCrashed) {
			t.Fatalf("flat unloaded core 2 under %v did not crash: %v", cores, err)
		}
		return m.Monotonic()
	}
	onFirst, onSecond := crashAt([]int{0}), crashAt([]int{1})
	if onFirst == onSecond {
		t.Fatalf("unloaded core 2 crashed at %s under both loaded sets", onFirst)
	}
	if diff := cmp.Diff(onFirst, crashAt([]int{0})); diff != "" {
		t.Fatalf("retried start (-first +retry): %s", diff)
	}
	if diff := cmp.Diff(crashAt([]int{0, 1}), crashAt([]int{1, 0})); diff != "" {
		t.Fatalf("loaded order changed the draw (-sorted +reversed): %s", diff)
	}
}

func TestJointCrashMachineEvidence(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name string
		key  string
		core *int
	}{
		{name: "default"},
		{name: "misleading core zero", key: "crash_mce_core = 0", core: new(0)},
		{name: "misleading core one", key: "crash_mce_core = 1", core: new(1)},
		{name: "negative core", key: "crash_mce_core = -1"},
		{name: "outside topology", key: "crash_mce_core = 2"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "machine.toml")
			content := fmt.Sprintf(`cores = 2
bios = [-20, -20]
[model]
crash_mce = 1
core_local_bank = 0
[[core]]
id = 0
alone = [-30, -30, -30, -30, -30]
together = [-30, -30, -30, -30, -30, -30, -30]
[[core]]
id = 1
alone = [-30, -30, -30, -30, -30]
together = [-30, -30, -30, -30, -30, -30, -30]
[[joint]]
members = { "0" = -20, "1" = -20 }
rate = 1000000
%s
`, tc.key)
			if err := os.WriteFile(path, []byte(content), 0600); err != nil {
				t.Fatal(err)
			}
			cfg, err := LoadMachine(path)
			if tc.key != "" && tc.core == nil {
				if err == nil || !strings.Contains(err.Error(), "joint crash MCE core") {
					t.Fatalf("invalid MCE core error: %v", err)
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			m := newMachine(t, cfg)
			if _, err := runSpec(t, m, "0001", machine.R7, machine.PickWorkload(machine.R7, 0), []int{1, 0}, time.Second, nil); !errors.Is(err, machine.ErrCrashed) {
				t.Fatalf("joint crash: %v", err)
			}
			m.Reboot()
			boot, _ := m.Seams().Host.BootID()
			mces, err := m.Seams().Kernel.MCEs(boot, 0)
			if err != nil {
				t.Fatal(err)
			}
			if tc.core == nil {
				if len(mces) != 0 {
					t.Fatalf("default joint crash invented evidence: %+v", mces)
				}
				return
			}
			if len(mces) != 1 || mces[0].Core != *tc.core || mces[0].CPU != *tc.core || mces[0].BankType != machine.LoadStore || mces[0].Corrected {
				t.Fatalf("misleading core %d MCE: %+v", *tc.core, mces)
			}
		})
	}
}

func TestScriptsAndClocks(t *testing.T) {
	m := newMachine(t, Config{Cores: 2, Limits: flat(2, -50, -50), Script: map[string]Outcome{
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

func TestScriptCrashResetAndBackendReport(t *testing.T) {
	m := newMachine(t, Config{Cores: 2, Script: map[string]Outcome{
		"0001": {Signal: machine.ComputationError, Core: 0, AtS: 2},
		"0002": {Signal: machine.Crash, Core: 0, AtS: 3, Reset: machine.ResetCPUShutdown},
	}})
	report := &signalRecorder{}
	res, err := runSpec(t, m, "0001", machine.R1, machine.PickWorkload(machine.R1, 0), []int{0}, time.Minute, report)
	if err != nil || res.Core != 0 || res.Ran != 2*time.Second {
		t.Fatalf("backend result = %+v, %v", res, err)
	}
	if diff := cmp.Diff([]machine.Signal{machine.ComputationError}, report.signals); diff != "" {
		t.Fatal(diff)
	}
	if _, err := runSpec(t, m, "0002", machine.R1, machine.PickWorkload(machine.R1, 0), []int{0}, time.Minute, nil); !errors.Is(err, machine.ErrCrashed) {
		t.Fatalf("script crash = %v", err)
	}
	m.Reboot()
	boot, err := m.Seams().Host.BootID()
	if err != nil {
		t.Fatal(err)
	}
	reason, err := m.Seams().Kernel.ResetReason(boot)
	if err != nil || reason.Kind != machine.ResetCPUShutdown {
		t.Fatalf("script reset = %+v, %v", reason, err)
	}
}

func TestWeightedCrashReset(t *testing.T) {
	model := sharp(machine.Crash)
	model.Reset = map[machine.ResetKind]float64{machine.ResetSyncFlood: 1}
	model.CrashMCE = 0
	m := newMachine(t, Config{Cores: 2, BIOS: []int{-11, 0}, Limits: flat(2, -10, -10), Model: model})
	if _, err := runSpec(t, m, "0001", machine.R1, machine.PickWorkload(machine.R1, 0), []int{0}, time.Second, nil); !errors.Is(err, machine.ErrCrashed) {
		t.Fatalf("weighted crash: %v", err)
	}
	m.Reboot()
	boot, _ := m.Seams().Host.BootID()
	if reason, err := m.Seams().Kernel.ResetReason(boot); err != nil || reason.Kind != machine.ResetSyncFlood {
		t.Fatalf("weighted reset reason %+v %v", reason, err)
	}
}

func TestCrashResetWithoutWeights(t *testing.T) {
	for _, weights := range []map[machine.ResetKind]float64{nil, {machine.ResetThermalTrip: 0}} {
		model := sharp(machine.Crash)
		model.Reset, model.CrashMCE = weights, 0
		m := newMachine(t, Config{Cores: 2, BIOS: []int{-11, 0}, Limits: flat(2, -10, -10), Model: model})
		if _, err := runSpec(t, m, "0001", machine.R1, machine.PickWorkload(machine.R1, 0), []int{0}, time.Minute, nil); !errors.Is(err, machine.ErrCrashed) {
			t.Fatalf("crash = %v", err)
		}
		m.Reboot()
		boot, err := m.Seams().Host.BootID()
		if err != nil {
			t.Fatal(err)
		}
		reason, err := m.Seams().Kernel.ResetReason(boot)
		if err != nil || reason.Kind != machine.ResetWatchdog {
			t.Fatalf("fallback reset = %+v, %v", reason, err)
		}
	}
}
