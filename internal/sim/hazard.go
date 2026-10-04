package sim

import (
	"math"
	"slices"

	"github.com/shgew/togi/internal/machine"
)

// Hazard returns steady-state failures per second, without onset boosts or joint delays.
// Profile must contain one register offset for each machine core.
func (m *Machine) Hazard(profile []int, spec machine.TrialSpec) float64 {
	var rate float64
	for core := range m.limits {
		rate += m.coreRate(profile, spec, core)
	}
	if m.sharedR7(spec) {
		for _, r := range m.voltageState(profile, spec).rates {
			rate += r
		}
		rate += m.cfg.SharedVoltage.BackgroundRate
	} else {
		for _, joint := range m.cfg.Joints {
			rate += m.jointRate(profile, spec.Regime, joint)
		}
	}
	for ccd := range 2 {
		rate += m.ccdRate(profile, spec, ccd)
	}
	return rate
}

// FailureProbability integrates the same hazards as a simulated start, including
// onset boosts, unloaded-core hazards and delayed joints, without drawing RNG.
func (m *Machine) FailureProbability(profile []int, spec machine.TrialSpec) float64 {
	exposure := func(after float64) float64 {
		duration := spec.Duration.Seconds()
		return max(0, duration-after) + max(0, m.model.OnsetBoost)*max(0, min(duration, m.model.OnsetS)-after)
	}
	var hazard float64
	for core := range m.limits {
		hazard += m.coreRate(profile, spec, core) * exposure(0)
	}
	if m.sharedR7(spec) {
		for _, rate := range m.voltageState(profile, spec).rates {
			hazard += rate * exposure(0)
		}
		hazard += m.cfg.SharedVoltage.BackgroundRate * exposure(0)
	} else {
		for _, joint := range m.cfg.Joints {
			hazard += m.jointRate(profile, spec.Regime, joint) * exposure(joint.AfterS)
		}
	}
	for ccd := range 2 {
		hazard += m.ccdRate(profile, spec, ccd) * exposure(0)
	}
	return -math.Expm1(-hazard)
}

func (m *Machine) coreRate(profile []int, spec machine.TrialSpec, core int) float64 {
	loaded := slices.Contains(spec.Cores, core)
	if loaded && m.sharedR7(spec) {
		return 0
	}
	limit := m.limit(profile, core, spec.Regime, spec.Workload.ID)
	if !loaded {
		if m.limits[core].Idle == nil && m.limits[core].Flat <= 0 {
			return 0
		}
		if m.limits[core].Idle != nil {
			limit = *m.limits[core].Idle
		}
	}
	rate := m.limits[core].Flat
	if loaded || m.limits[core].Idle != nil {
		d := limit - profile[core]
		if d >= 1 {
			rate += m.model.PastLimitRate * math.Pow(m.model.Growth, float64(d-1))
		} else if loaded {
			rate += m.model.NearLimitRate
		}
	}
	if profile[core] == 0 {
		rate -= m.limits[core].Flat
	}
	return rate
}

func (m *Machine) jointRate(profile []int, regime machine.Regime, joint Joint) float64 {
	if len(joint.Regimes) > 0 && !slices.Contains(joint.Regimes, regime) {
		return 0
	}
	if len(joint.Members) == 0 {
		return 0
	}
	for core, offset := range joint.Members {
		if profile[core] > offset {
			return 0
		}
	}
	if joint.Rate == 0 {
		return m.model.PastLimitRate
	}
	return joint.Rate
}

func (m *Machine) ccdRate(profile []int, spec machine.TrialSpec, ccd int) float64 {
	c := m.cfg.CCD
	if c == nil || spec.Regime != machine.R7 || m.sharedR7(spec) {
		return 0
	}
	size := m.cfg.Cores / 2
	loaded := false
	for _, core := range spec.Cores {
		loaded = loaded || core/size == ccd
	}
	if !loaded {
		return 0
	}
	// Existing joint explanations take precedence over extrapolation on this CCD.
	for _, joint := range m.cfg.Joints {
		if m.jointRate(profile, spec.Regime, joint) <= 0 {
			continue
		}
		for core := range joint.Members {
			if core/size == ccd {
				return 0
			}
		}
	}
	depth := 0
	for core := ccd * size; core < (ccd+1)*size; core++ {
		depth -= profile[core]
	}
	return math.Exp(c.LogRate + c.Effect[ccd] + c.Slope*(float64(depth)/float64(size)-25))
}
