package sim

import (
	"context"
	"errors"
	"math"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/requests"
)

func voltageConfig(t *testing.T) Config {
	t.Helper()
	cfg, err := LoadMachine("../../tools/bench/machines/shared-voltage.toml")
	if err != nil {
		t.Fatal(err)
	}
	return cfg
}

func voltageSpec(cores ...int) machine.TrialSpec {
	return machine.TrialSpec{ID: "0001", Regime: machine.R7, Workload: machine.PickWorkload(machine.R7, 0), Cores: cores, Condition: machine.Together, Duration: 120 * time.Second}
}

func TestSharedVoltageTopRequester(t *testing.T) {
	m := newMachine(t, voltageConfig(t))
	profile := slices.Repeat([]int{-35}, 16)
	profile[6] = -40 // Leave a request gap larger than the partial's clock boost.
	spec := voltageSpec(0, 1, 2, 3, 4, 5, 6, 7)
	full := m.voltageState(profile, spec)
	want := float64(full.requests[7])
	if math.Abs(full.voltage-want) > 1e-7 {
		t.Fatalf("shared voltage %.9f != top request %.9f", full.voltage, want)
	}
	spec.Cores = spec.Cores[:7]
	partial := m.voltageState(profile, spec)
	if partial.voltage >= full.voltage || partial.requests[7] != float32(m.cfg.SharedVoltage.IdleV) {
		t.Fatalf("idling top requester: full=%+v partial=%+v", full, partial)
	}
	// An unloaded core at offset 0 cannot set the rail.
	profile[7] = 0
	if diff := cmp.Diff(partial, m.voltageState(profile, spec), cmp.AllowUnexported(voltageState{})); diff != "" {
		t.Fatal(diff)
	}
	// Across CCDs, the higher request still wins.
	spec.Cores = append(spec.Cores, 10)
	both := m.voltageState(profile, spec)
	if both.voltage <= partial.voltage || math.Abs(both.voltage-float64(both.requests[10])) > 1e-7 {
		t.Fatalf("cross-CCD rail is not set by core 10: %+v", both)
	}
}

func TestSharedVoltageClockShape(t *testing.T) {
	m := newMachine(t, voltageConfig(t))
	profile := slices.Repeat([]int{-35}, 16)
	full := voltageSpec(0, 1, 2, 3, 4, 5, 6, 7)
	partial := full
	partial.Cores = full.Cores[:7]
	if diff := cmp.Diff([2]int{5240, 0}, m.voltageState(profile, full).clocks); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff([2]int{5270, 0}, m.voltageState(profile, partial).clocks); diff != "" {
		t.Fatal(diff)
	}
	full.Cores = make([]int, 16)
	for core := range full.Cores {
		full.Cores[core] = core
	}
	full.Workload = machine.PickWorkload(machine.R7, 1)
	if diff := cmp.Diff([2]int{4799, 4799}, m.voltageState(profile, full).clocks); diff != "" {
		t.Fatal(diff)
	}
	for core := range 8 {
		profile[core] = 0
	}
	if diff := cmp.Diff([2]int{4329, 4889}, m.voltageState(profile, full).clocks); diff != "" {
		t.Fatal(diff)
	}
	m.cfg.SharedVoltage.ThermalLimitW = 180
	limited := m.voltageState(profile, full)
	if limited.clocks[0] >= 4329 || limited.clocks[1] >= 4889 {
		t.Fatalf("thermal limit did not reduce clocks: %+v", limited.clocks)
	}
}

func TestSharedVoltageCore11NeedsSupport(t *testing.T) {
	m := newMachine(t, voltageConfig(t))
	profile := []int{-31, -38, -37, -34, -35, -36, -45, -42, -48, -47, -38, -44, -50, -42, -50, -50}
	spec := voltageSpec(8, 9, 11, 12, 14, 15)
	spec.Workload = machine.PickWorkload(machine.R7, 1)
	unsupported := m.voltageState(profile, spec)
	full := spec
	full.Cores = []int{8, 9, 10, 11, 12, 13, 14, 15}
	fullState := m.voltageState(profile, full)
	if fullState.voltage <= m.cfg.SharedVoltage.Workload[spec.Workload.ID].Core[11].ThresholdV {
		t.Fatal("the full CCD must supply core 11's threshold")
	}
	for _, supporter := range []int{10, 13} {
		supported := spec
		supported.Cores = append(slices.Clone(spec.Cores), supporter)
		state := m.voltageState(profile, supported)
		threshold := m.cfg.SharedVoltage.Workload[spec.Workload.ID].Core[11].ThresholdV
		if unsupported.voltage >= threshold || state.voltage <= threshold || unsupported.rates[11] <= state.rates[11]*10 {
			t.Fatalf("core %d support: unsupported=%+v supported=%+v", supporter, unsupported, state)
		}
		if got := m.voltageSignal(spec, 11); got != machine.ComputationError {
			t.Fatalf("core 11 signal = %s", got)
		}
	}
}

func TestSharedVoltageDrawsAndScores(t *testing.T) {
	cfg := voltageConfig(t)
	cfg.Limits = flat(16, -50, -50)
	cfg.CCD = &CCD{LogRate: 10}
	cfg.Joints = []Joint{{Members: map[int]int{0: 0}, Regimes: []machine.Regime{machine.R7}, Rate: 100}}
	cfg.Model.CrashMCE = 1
	cfg.Model.Signals = map[machine.Signal]float64{machine.Stall: 1}
	for key, w := range cfg.SharedVoltage.Workload {
		w.FullMHz = [2]float64{w.ReferenceMHz + 100, w.ReferenceMHz + 100}
		w.IdleGainMHz, w.PackageMHzPerW, w.BalanceMHzPerW = 0, 0, 0
		for core := range w.Core {
			w.Core[core].BaseV = 1.2
			w.Core[core].ThresholdV = 1.0
			w.Core[core].ClockVPer100MHz = .03
			w.Core[core].ThresholdClockVPer100MHz = .03
			w.Core[core].Signals = nil
		}
		w.Core[0].ThresholdV = 1.2
		cfg.SharedVoltage.Workload[key] = w
	}
	m := newMachine(t, cfg)
	spec := voltageSpec(0, 1)
	rate := m.Hazard(m.regs, spec)
	if math.Abs(rate-cfg.SharedVoltage.Rate) > 1e-12 {
		t.Fatalf("shared hazard must replace loaded legacy hazards: %g", rate)
	}
	want := -math.Expm1(-rate * spec.Duration.Seconds())
	if got := m.FailureProbability(m.regs, spec); math.Abs(got-want) > 1e-12 {
		t.Fatalf("probability = %g, want %g", got, want)
	}
	var failures int
	for index := range 2000 {
		spec.Index = index
		r, err := m.Seams().Trials.Start(context.Background(), spec)
		if err != nil {
			t.Fatal(err)
		}
		_, err = r.Wait(context.Background(), nil)
		if errors.Is(err, machine.ErrCrashed) {
			failures++
			m.Reboot()
			if len(m.queued) != 0 || len(m.logs[m.bootID]) != 0 {
				t.Fatal("shared voltage crash fabricated MCE evidence")
			}
		} else if err != nil {
			t.Fatal(err)
		}
	}
	if got := float64(failures) / 2000; math.Abs(got-want) > .025 {
		t.Fatalf("draw failure rate %g != probability %g", got, want)
	}
	profile := slices.Repeat([]int{-10}, 16)
	if m.FailureProbability(profile, spec) <= want*5 {
		t.Fatal("voltage below threshold must fail more often")
	}
	// Single-core R7 and other regimes still use the old hazards.
	spec.Cores = []int{0}
	if m.Hazard(m.regs, spec) < 100 {
		t.Fatal("single-core R7 did not retain legacy joints")
	}
}

func TestSharedVoltageSamplesAndDeterminism(t *testing.T) {
	cfg := voltageConfig(t)
	cfg.Script = map[string]Outcome{"0001": {}}
	var memory []machine.TrialConditions
	for _, fileBacked := range []bool{false, true} {
		m := newMachine(t, cfg)
		if fileBacked {
			m.SetSamplesDir(t.TempDir())
		}
		_, err := runSpec(t, m, "0001", machine.R7, machine.PickWorkload(machine.R7, 0), []int{0, 1, 7}, 30*time.Second, nil)
		if err != nil {
			t.Fatal(err)
		}
		m.Reboot()
		samples := slices.Collect(m.Seams().Trials.Samples("0001"))
		if !fileBacked {
			memory = samples
		} else if diff := cmp.Diff(memory, samples); diff != "" {
			t.Fatal(diff)
		}
		for _, sample := range samples {
			if sample.PMTable == nil || sample.CoreMHz.Len() != 3 || sample.PMTable.VoltageRequestV[8] != .8 {
				t.Fatalf("incomplete lanes: %+v", sample)
			}
		}
		telemetry, ok := requests.Summarize(m.Seams().Trials.Samples("0001"), []int{0, 1, 7}, func(core int) int { return core / 8 })
		if !ok || len(telemetry.Requests) != 3 {
			t.Fatalf("request summary = %+v, %v", telemetry, ok)
		}
		if diff := cmp.Diff([]int{7}, telemetry.TopRequesters); diff != "" {
			t.Fatal(diff)
		}
		if diff := cmp.Diff(map[int]int{0: 5390}, telemetry.CCDMHz); diff != "" {
			t.Fatal(diff)
		}
	}
	m := newMachine(t, cfg)
	for index := range 100 {
		spec := voltageSpec(0, 1)
		spec.Index = index
		a := m.failureDraw(.1, 0, spec, 0, "voltage")
		if b := newMachine(t, cfg).failureDraw(.1, 0, spec, 0, "voltage"); a != b {
			t.Fatalf("same seed changed draw: %s != %s", a, b)
		}
	}
}

func TestSharedVoltageValidation(t *testing.T) {
	for _, name := range []string{"topology", "missing workload", "missing core", "nonfinite", "negative slope", "bad signal"} {
		t.Run(name, func(t *testing.T) {
			cfg := voltageConfig(t)
			key := machine.PickWorkload(machine.R7, 0).ID
			w := cfg.SharedVoltage.Workload[key]
			switch name {
			case "topology":
				cfg.Cores = 8
			case "missing workload":
				delete(cfg.SharedVoltage.Workload, key)
			case "missing core":
				w.Core = w.Core[:15]
			case "nonfinite":
				cfg.SharedVoltage.MarginV = math.NaN()
			case "negative slope":
				w.Core[0].CountV = -1
			case "bad signal":
				w.Core[0].Signals = map[machine.Signal]float64{machine.Crash: math.Inf(1)}
			}
			if name != "missing workload" {
				cfg.SharedVoltage.Workload[key] = w
			}
			if _, err := New(cfg); err == nil {
				t.Fatal("invalid shared_voltage accepted")
			}
		})
	}
}

func TestSharedVoltageSamplesStayLazy(t *testing.T) {
	m := newMachine(t, voltageConfig(t))
	r := running{m: m, spec: voltageSpec(0, 1)}
	allocations := func(duration time.Duration) float64 {
		return testing.AllocsPerRun(2, func() {
			if err := r.sampleConditions(duration, -1); err != nil {
				t.Fatal(err)
			}
		})
	}
	short := allocations(30 * time.Second)
	long := allocations(time.Hour)
	if long > short+1 {
		t.Fatalf("retained allocations depend on duration: short=%g long=%g", short, long)
	}
	count := 0
	for sample := range m.Seams().Trials.Samples(r.spec.ID) {
		count++
		if sample.ElapsedMS != 1000 || sample.PMTable == nil {
			t.Fatalf("first lazy sample: %+v", sample)
		}
		break
	}
	if count != 1 {
		t.Fatalf("early-stop iterator yielded %d samples", count)
	}
}

func TestSharedVoltageThresholdClock(t *testing.T) {
	cfg := voltageConfig(t)
	cfg.Limits = flat(16, -50, -50)
	key := machine.PickWorkload(machine.R7, 0).ID
	w := cfg.SharedVoltage.Workload[key]
	w.IdleGainMHz, w.PackageMHzPerW, w.BalanceMHzPerW = 0, 0, 0
	for core := range w.Core {
		w.Core[core].BaseV = 1.2
		w.Core[core].ThresholdV = 1.074
		w.Core[core].ClockVPer100MHz = .03
		w.Core[core].ThresholdClockVPer100MHz = .03
	}
	profile := slices.Repeat([]int{-35}, 16)
	spec := voltageSpec(0, 1)
	var reference float64
	for _, delta := range []float64{0, -100, 100} {
		w.FullMHz = [2]float64{w.ReferenceMHz + delta, w.ReferenceMHz + delta}
		cfg.SharedVoltage.Workload[key] = w
		m := newMachine(t, cfg)
		got := m.Hazard(profile, spec)
		if delta == 0 {
			reference = got
		}
		if math.Abs(got-reference) > 1e-12 {
			t.Fatalf("matching clock slopes changed hazard at %+g MHz: %g != %g", delta, got, reference)
		}
		want := -math.Expm1(-reference * spec.Duration.Seconds())
		if got := m.FailureProbability(profile, spec); math.Abs(got-want) > 1e-12 {
			t.Fatalf("matching clock slopes changed probability at %+g MHz: %g != %g", delta, got, want)
		}
	}
	for core := range w.Core {
		w.Core[core].ThresholdClockVPer100MHz = .04
	}
	cfg.SharedVoltage.Workload[key] = w
	if got := newMachine(t, cfg).Hazard(profile, spec); got <= reference {
		t.Fatalf("faster-growing required voltage did not increase hazard: %g <= %g", got, reference)
	}
}

func TestSharedVoltageDefaultThresholdClock(t *testing.T) {
	m := newMachine(t, voltageConfig(t))
	profile := slices.Repeat([]int{-35}, 16)
	for _, spec := range []machine.TrialSpec{voltageSpec(0, 1), voltageSpec(8, 9, 10, 11, 12, 13, 14, 15)} {
		state := m.voltageState(profile, spec)
		w := m.cfg.SharedVoltage.Workload[spec.Workload.ID]
		for _, core := range spec.Cores {
			if w.Core[core].ThresholdClockVPer100MHz != 0 {
				t.Fatal("omitted threshold clock coefficient is not zero")
			}
			x := (w.Core[core].ThresholdV - state.voltage) / m.cfg.SharedVoltage.MarginV
			want := m.cfg.SharedVoltage.Rate * (max(x, 0) + math.Log1p(math.Exp(-math.Abs(x)))) / math.Ln2
			if got := state.rates[core]; got != want {
				t.Fatalf("default coefficient changed legacy core %d hazard: %g != %g", core, got, want)
			}
		}
	}
}

func TestR7FailureShare(t *testing.T) {
	m := newMachine(t, voltageConfig(t))
	profile := slices.Repeat([]int{-35}, 16)
	spec := voltageSpec(0, 1, 7)
	state := m.voltageState(profile, spec)
	for _, idleRate := range []float64{0, .01} {
		m.limits[8].Flat = idleRate
		total := m.Hazard(profile, spec)
		for _, core := range spec.Cores {
			want := state.rates[core] / total
			if got := m.R7FailureShare(profile, spec, core); math.Abs(got-want) > 1e-12 {
				t.Fatalf("core %d failure share = %g, want %g (idle rate %g)", core, got, want, idleRate)
			}
		}
	}
	for _, core := range []int{-1, 8, 16} {
		if got := m.R7FailureShare(profile, spec, core); got != 0 {
			t.Fatalf("unloaded/invalid core %d share = %g", core, got)
		}
	}
	if allocations := testing.AllocsPerRun(10, func() { m.R7FailureShare(profile, spec, 0) }); allocations != 0 {
		t.Fatalf("failure share allocated %g times", allocations)
	}
	for _, nonshared := range []machine.TrialSpec{voltageSpec(0), {Regime: machine.R2, Cores: []int{0, 1}}} {
		if got := m.R7FailureShare(profile, nonshared, 0); got != 0 {
			t.Fatalf("nonshared trial failure share = %g", got)
		}
	}
	legacy := newMachine(t, Config{Cores: 16})
	if got := legacy.R7FailureShare(profile, spec, 0); got != 0 {
		t.Fatalf("legacy machine failure share = %g", got)
	}
	cfg := voltageConfig(t)
	for _, w := range cfg.SharedVoltage.Workload {
		for core := range w.Core {
			w.Core[core].BaseV = 100
		}
	}
	zero := newMachine(t, cfg)
	if got := zero.R7FailureShare(profile, spec, 0); got != 0 {
		t.Fatalf("zero-hazard failure share = %g", got)
	}
}

func TestSharedVoltageThresholdClockValidation(t *testing.T) {
	for _, coefficient := range []float64{-1, math.NaN(), math.Inf(1), math.Inf(-1)} {
		cfg := voltageConfig(t)
		key := machine.PickWorkload(machine.R7, 0).ID
		cfg.SharedVoltage.Workload[key].Core[0].ThresholdClockVPer100MHz = coefficient
		if _, err := New(cfg); err == nil {
			t.Fatalf("invalid required-voltage coefficient %g accepted", coefficient)
		}
	}
	for _, coefficient := range []float64{0, .027, 2} {
		cfg := voltageConfig(t)
		key := machine.PickWorkload(machine.R7, 0).ID
		cfg.SharedVoltage.Workload[key].Core[0].ThresholdClockVPer100MHz = coefficient
		if _, err := New(cfg); err != nil {
			t.Fatalf("finite nonnegative required-voltage coefficient %g rejected: %v", coefficient, err)
		}
	}
}

func TestSharedVoltageThresholdClockDecodeAndOwnership(t *testing.T) {
	source, err := os.ReadFile("../../tools/bench/machines/shared-voltage.toml")
	if err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(t.TempDir(), "machine.toml")
	content := strings.ReplaceAll(string(source), "threshold_v =", "threshold_clock_v_per_100mhz = 0.027\nthreshold_v =")
	content = strings.ReplaceAll(content, "[shared_voltage]", "[shared_voltage]\nbackground_rate = 0.001")
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
	cfg, err := LoadMachine(path)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.SharedVoltage.BackgroundRate != .001 {
		t.Fatal("background_rate not decoded")
	}
	m := newMachine(t, cfg)
	profile := slices.Repeat([]int{-35}, 16)
	spec := voltageSpec(0, 1)
	before := m.voltageState(profile, spec).rates[0]
	for key, w := range cfg.SharedVoltage.Workload {
		for core := range w.Core {
			if w.Core[core].ThresholdClockVPer100MHz != .027 {
				t.Fatalf("workload %s core %d coefficient not decoded", key, core)
			}
			w.Core[core].ThresholdClockVPer100MHz = .5
		}
	}
	if got := m.voltageState(profile, spec).rates[0]; got != before {
		t.Fatalf("caller mutation changed owned coefficient: %g != %g", got, before)
	}
}

func backgroundVoltageConfig(t *testing.T) Config {
	t.Helper()
	cfg := voltageConfig(t)
	cfg.Limits = flat(16, -50, -50)
	cfg.SharedVoltage.BackgroundRate = .001
	for _, w := range cfg.SharedVoltage.Workload {
		for core := range w.Core {
			w.Core[core].BaseV = 100 // Voltage failures underflow to zero, isolating background.
		}
	}
	return cfg
}

func TestSharedVoltageBackgroundHazard(t *testing.T) {
	cfg := backgroundVoltageConfig(t)
	cfg.Model.OnsetBoost, cfg.Model.OnsetS = 2, 10
	m := newMachine(t, cfg)
	for _, offset := range []int{0, -35, -50} {
		profile := slices.Repeat([]int{offset}, 16)
		for _, workload := range machine.Workloads(machine.R7) {
			for _, cores := range [][]int{{0, 1}, {0, 1, 2, 3, 4, 5, 6, 7}, {0, 1, 8, 9}} {
				spec := voltageSpec(cores...)
				spec.Workload = workload
				if got := m.Hazard(profile, spec); got != cfg.SharedVoltage.BackgroundRate {
					t.Fatalf("background hazard offset=%d load=%v workload=%s: %g", offset, cores, workload.ID, got)
				}
				for _, duration := range []time.Duration{5 * time.Second, 120 * time.Second} {
					spec.Duration = duration
					exposure := duration.Seconds() + 2*min(duration.Seconds(), 10)
					want := -math.Expm1(-cfg.SharedVoltage.BackgroundRate * exposure)
					if got := m.FailureProbability(profile, spec); got != want {
						t.Fatalf("background onset probability = %g, want %g", got, want)
					}
				}
				if got := m.R7FailureShare(profile, spec, cores[0]); got != 0 {
					t.Fatalf("background-only failure attributed to core: %g", got)
				}
			}
		}
	}
	profile := make([]int, 16)
	for _, spec := range []machine.TrialSpec{voltageSpec(0), {Regime: machine.R2, Workload: machine.PickWorkload(machine.R2, 0), Cores: []int{0, 1}}} {
		if got := m.Hazard(profile, spec); got != 0 {
			t.Fatalf("background applied outside shared multi-core R7: %g", got)
		}
	}
	cfg.SharedVoltage.BackgroundRate = 0
	zero := newMachine(t, cfg)
	if got := zero.Hazard(profile, voltageSpec(0, 1)); got != 0 {
		t.Fatalf("zero background changed hazard: %g", got)
	}
}

func TestSharedVoltageBackgroundDraws(t *testing.T) {
	cfg := backgroundVoltageConfig(t)
	cfg.Model.OnsetBoost, cfg.Model.OnsetS = 2, 10
	cfg.Model.CrashMCE = 1
	cfg.Model.Signals = map[machine.Signal]float64{machine.ComputationError: 1}
	m := newMachine(t, cfg)
	spec := voltageSpec(0, 1)
	var failures int
	for index := range 2000 {
		spec.Index = index
		wantAt := m.failureDraw(cfg.SharedVoltage.BackgroundRate, 0, spec, 0, "voltage-background")
		start := m.now
		r, err := m.Seams().Trials.Start(context.Background(), spec)
		if err != nil {
			t.Fatal(err)
		}
		_, err = r.Wait(context.Background(), nil)
		if m.now.Sub(start) != wantAt {
			t.Fatalf("background draw time = %s, want %s", m.now.Sub(start), wantAt)
		}
		if wantAt < spec.Duration {
			if !errors.Is(err, machine.ErrCrashed) {
				t.Fatalf("background draw did not crash: %v", err)
			}
			failures++
			if m.samples.stalledCore != -1 {
				t.Fatalf("background fabricated stalled-core attribution: %d", m.samples.stalledCore)
			}
			m.Reboot()
			if len(m.queued) != 0 || len(m.logs[m.bootID]) != 0 {
				t.Fatal("background crash fabricated MCE evidence")
			}
		} else if err != nil {
			t.Fatal(err)
		}
	}
	want := m.FailureProbability(m.regs, spec)
	if got := float64(failures) / 2000; math.Abs(got-want) > .025 {
		t.Fatalf("background draw rate %g != probability %g", got, want)
	}
}

func TestSharedVoltageBackgroundFailureShare(t *testing.T) {
	cfg := voltageConfig(t)
	cfg.SharedVoltage.BackgroundRate = .01
	m := newMachine(t, cfg)
	profile := slices.Repeat([]int{-35}, 16)
	spec := voltageSpec(0, 1)
	state := m.voltageState(profile, spec)
	var shared float64
	for _, rate := range state.rates {
		shared += rate
	}
	for _, core := range spec.Cores {
		want := state.rates[core] / (shared + cfg.SharedVoltage.BackgroundRate)
		if got := m.R7FailureShare(profile, spec, core); math.Abs(got-want) > 1e-12 {
			t.Fatalf("background-inclusive failure share = %g, want %g", got, want)
		}
	}
}

func TestSharedVoltageBackgroundValidation(t *testing.T) {
	for _, rate := range []float64{-1, math.NaN(), math.Inf(1), math.Inf(-1)} {
		cfg := voltageConfig(t)
		cfg.SharedVoltage.BackgroundRate = rate
		if _, err := New(cfg); err == nil {
			t.Fatalf("invalid background rate %g accepted", rate)
		}
	}
	for _, rate := range []float64{0, .001, 2} {
		cfg := voltageConfig(t)
		cfg.SharedVoltage.BackgroundRate = rate
		if _, err := New(cfg); err != nil {
			t.Fatalf("finite nonnegative background rate %g rejected: %v", rate, err)
		}
	}
}

func TestSharedVoltageBackgroundReplayBypass(t *testing.T) {
	cfg := backgroundVoltageConfig(t)
	cfg.SharedVoltage.BackgroundRate = 100
	cfg.BIOSContext = defaultBIOSContext
	profile := make([]int, 16)
	spec := voltageSpec(0, 1)
	replay, err := NewReplay(cfg.BIOSContext, []ReplayFact{{
		Context: cfg.BIOSContext,
		Class:   journal.TrialClass{Regime: spec.Regime, Workload: spec.Workload.ID, Cores: spec.Cores, DurationS: int(spec.Duration.Seconds())},
		Profile: profile, Outcome: journal.OutcomePass, DurationS: int(spec.Duration.Seconds()),
	}})
	if err != nil {
		t.Fatal(err)
	}
	cfg.Replay = replay
	m := newMachine(t, cfg)
	r, err := m.Seams().Trials.Start(context.Background(), spec)
	if err != nil {
		t.Fatal(err)
	}
	result, err := r.Wait(context.Background(), nil)
	if err != nil || result.Ran != spec.Duration || result.Signal != "" {
		t.Fatalf("background overrode replayed pass: %+v, %v", result, err)
	}
}
