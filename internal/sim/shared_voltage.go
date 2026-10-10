package sim

import (
	"errors"
	"fmt"
	"maps"
	"math"
	"slices"

	"github.com/shgew/togi/internal/machine"
)

// SharedVoltage models the common rail, with independent loaded-core hazards.
type SharedVoltage struct {
	IdleV          float64                    `json:"idle_v"`
	MarginV        float64                    `json:"margin_v"`
	Rate           float64                    `json:"rate"`
	BackgroundRate float64                    `json:"background_rate"`
	PowerLimitW    float64                    `json:"power_limit_w"`
	ThermalLimitW  float64                    `json:"thermal_limit_w"`
	Workload       map[string]VoltageWorkload `json:"workload"`
}

type VoltageWorkload struct {
	ReferenceMHz        float64       `json:"reference_mhz"`
	FullMHz             [2]float64    `json:"full_mhz"`
	IdleGainMHz         float64       `json:"idle_gain_mhz"`
	WattsPerCore        float64       `json:"watts_per_core"`
	OffsetWattsPerCount float64       `json:"offset_watts_per_count"`
	PackageMHzPerW      float64       `json:"package_mhz_per_w"`
	BalanceMHzPerW      float64       `json:"balance_mhz_per_w"`
	Core                []VoltageCore `json:"core"`
}

type VoltageCore struct {
	BaseV                    float64                    `json:"base_v"`
	ThresholdV               float64                    `json:"threshold_v"`
	CountV                   float64                    `json:"count_v"`
	ClockVPer100MHz          float64                    `json:"clock_v_per_100mhz"`
	ThresholdClockVPer100MHz float64                    `json:"threshold_clock_v_per_100mhz"`
	Signals                  map[machine.Signal]float64 `json:"signals,omitzero"`
}

func normalizeVoltage(v *SharedVoltage, cores int) (*SharedVoltage, error) {
	if v == nil {
		return nil, nil
	}
	if cores != 16 {
		return nil, errors.New("shared_voltage requires 16 cores")
	}
	positive := func(x float64) bool { return x > 0 && !math.IsInf(x, 0) && !math.IsNaN(x) }
	nonnegative := func(x float64) bool { return x >= 0 && !math.IsInf(x, 0) && !math.IsNaN(x) }
	if !positive(v.IdleV) || !positive(v.MarginV) || !positive(v.Rate) || !positive(v.PowerLimitW) || !positive(v.ThermalLimitW) {
		return nil, errors.New("shared_voltage idle_v, margin_v, rate and limits must be finite and positive")
	}
	if !nonnegative(v.BackgroundRate) {
		return nil, errors.New("shared_voltage background_rate must be finite and nonnegative")
	}
	out := *v
	out.Workload = maps.Clone(v.Workload)
	for _, workload := range machine.Workloads(machine.R7) {
		w, ok := v.Workload[workload.ID]
		if !ok {
			return nil, fmt.Errorf("shared_voltage missing workload %s", workload.ID)
		}
		if !positive(w.ReferenceMHz) || !positive(w.FullMHz[0]) || !positive(w.FullMHz[1]) || !positive(w.WattsPerCore) || !nonnegative(w.IdleGainMHz) || !nonnegative(w.OffsetWattsPerCount) || !nonnegative(w.PackageMHzPerW) || !nonnegative(w.BalanceMHzPerW) {
			return nil, fmt.Errorf("shared_voltage workload %s has invalid clock or power parameters", workload.ID)
		}
		if len(w.Core) != cores {
			return nil, fmt.Errorf("shared_voltage workload %s needs %d cores in core-ID order", workload.ID, cores)
		}
		w.Core = slices.Clone(w.Core)
		for c := range w.Core {
			p := &w.Core[c]
			if p.CountV == 0 {
				p.CountV = .0036
			}
			if !positive(p.BaseV) || !positive(p.ThresholdV) || !positive(p.CountV) || !nonnegative(p.ClockVPer100MHz) || !nonnegative(p.ThresholdClockVPer100MHz) {
				return nil, fmt.Errorf("shared_voltage workload %s core %d has invalid voltage parameters", workload.ID, c)
			}
			if p.Signals != nil {
				if err := validateSignals(p.Signals); err != nil {
					return nil, fmt.Errorf("shared_voltage workload %s core %d: %w", workload.ID, c, err)
				}
				for _, weight := range p.Signals {
					if !nonnegative(weight) {
						return nil, errors.New("shared_voltage signal weights must be finite")
					}
				}
				p.Signals = maps.Clone(p.Signals)
			}
		}
		out.Workload[workload.ID] = w
	}
	if len(out.Workload) != len(machine.Workloads(machine.R7)) {
		return nil, errors.New("shared_voltage contains an unknown workload")
	}
	return &out, nil
}

func (m *hazards) sharedR7(spec machine.TrialSpec) bool {
	return m.cfg.SharedVoltage != nil && spec.Regime == machine.R7 && len(spec.Cores) > 1
}

// R7Requests returns the model's loaded-core requests without samples or RNG.
// It is available only for multi-core R7 loads on shared-voltage machines.
func (m *hazards) R7Requests(profile []int, spec machine.TrialSpec) ([16]float32, bool) {
	if !m.sharedR7(spec) {
		return [16]float32{}, false
	}
	return m.voltageState(profile, spec).requests, true
}

// R7FailureShare returns a loaded core's shared-voltage rate divided by the
// total steady-state hazard, including background and unloaded-core hazards.
// It returns zero outside multi-core shared-voltage R7, for an unloaded or
// invalid core, or when total hazard is zero. Profile needs one offset per core.
func (m *hazards) R7FailureShare(profile []int, spec machine.TrialSpec, core int) float64 {
	if !m.sharedR7(spec) || core < 0 || core >= len(m.limits) || !slices.Contains(spec.Cores, core) {
		return 0
	}
	state := m.voltageState(profile, spec)
	var total float64
	total += m.cfg.SharedVoltage.BackgroundRate
	for _, rate := range state.rates {
		total += rate
	}
	for c := range m.limits {
		total += m.coreRate(profile, spec, c)
	}
	if total == 0 {
		return 0
	}
	return state.rates[core] / total
}

type voltageState struct {
	requests [16]float32
	clocks   [2]int
	voltage  float64
	rates    [16]float64
}

func (m *hazards) voltageState(profile []int, spec machine.TrialSpec) voltageState {
	v := m.cfg.SharedVoltage
	w, ok := v.Workload[spec.Workload.ID]
	if !ok {
		// Non-R7 telemetry is illustrative; it does not change the legacy hazards.
		w = v.Workload[machine.PickWorkload(machine.R7, 0).ID]
	}
	var state voltageState
	var loaded [2]int
	var watts [2]float64
	for _, core := range spec.Cores {
		ccd := core / 8
		loaded[ccd]++
		watts[ccd] += max(0, w.WattsPerCore+w.OffsetWattsPerCount*float64(profile[core]+35))
	}
	pressure := max(0, watts[0]+watts[1]-min(v.PowerLimitW, v.ThermalLimitW))
	for ccd := range 2 {
		if loaded[ccd] == 0 {
			continue
		}
		mhz := w.FullMHz[ccd] + w.IdleGainMHz*float64(8-loaded[ccd]) - w.PackageMHzPerW*pressure
		if loaded[0] > 0 && loaded[1] > 0 {
			mhz -= w.BalanceMHzPerW * (watts[ccd] - (watts[0]+watts[1])/2)
		}
		state.clocks[ccd] = max(1, int(math.Round(mhz)))
	}
	for core := range 16 {
		state.requests[core] = float32(v.IdleV)
	}
	for _, core := range spec.Cores {
		p := w.Core[core]
		request := p.BaseV + p.CountV*float64(profile[core]) + p.ClockVPer100MHz*(float64(state.clocks[core/8])-w.ReferenceMHz)/100
		state.requests[core] = float32(request)
		state.voltage = max(state.voltage, request)
	}
	for _, core := range spec.Cores {
		// Softplus is smooth on both sides of the threshold and linear far below it.
		p := &w.Core[core]
		threshold := p.ThresholdV
		if p.ThresholdClockVPer100MHz != 0 {
			threshold += p.ThresholdClockVPer100MHz * (float64(state.clocks[core/8]) - w.ReferenceMHz) / 100
		}
		x := (threshold - state.voltage) / v.MarginV
		softplus := max(x, 0) + math.Log1p(math.Exp(-math.Abs(x)))
		state.rates[core] = v.Rate * softplus / math.Ln2
	}
	return state
}

func (m *Machine) voltageSignal(spec machine.TrialSpec, core int) machine.Signal {
	weights := m.cfg.SharedVoltage.Workload[spec.Workload.ID].Core[core].Signals
	if weights == nil {
		weights = m.signals(spec.Regime)
	}
	var total float64
	for _, signal := range signalOrder {
		total += weights[signal]
	}
	if m.trialRNG("voltage-signal", spec, core).Float64()*total < weights[machine.ComputationError] {
		return machine.ComputationError
	}
	return machine.Crash
}
