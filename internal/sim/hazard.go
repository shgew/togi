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
		for j := range m.cfg.Joints {
			rate += m.jointRate(profile, spec.Regime, j)
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
	duration := spec.Duration.Seconds()
	onsetBoost, onsetS := max(0, m.model.OnsetBoost), m.model.OnsetS
	exposure := func(after float64) float64 {
		return max(0, duration-after) + onsetBoost*max(0, min(duration, onsetS)-after)
	}
	always := exposure(0)
	var hazard float64
	var loadedBuf [64]bool
	var loaded []bool
	if len(m.limits) <= len(loadedBuf) {
		loaded = loadedBuf[:len(m.limits)]
	} else {
		loaded = make([]bool, len(m.limits))
	}
	for _, core := range spec.Cores {
		if core >= 0 && core < len(loaded) {
			loaded[core] = true
		}
	}
	shared := m.sharedR7(spec)
	nonzero := countNonzero(profile)
	for core := range m.limits {
		hazard += m.coreRateOf(profile, spec.Regime, spec.Workload.ID, core, loaded[core], shared, nonzero) * always
	}
	if shared {
		for _, rate := range m.voltageState(profile, spec).rates {
			hazard += rate * always
		}
		hazard += m.cfg.SharedVoltage.BackgroundRate * always
	} else {
		for j, joint := range m.cfg.Joints {
			if rate := m.jointRate(profile, spec.Regime, j); rate != 0 {
				hazard += rate * exposure(joint.AfterS)
			}
		}
	}
	if m.cfg.CCD != nil {
		for ccd := range 2 {
			hazard += m.ccdRate(profile, spec, ccd) * always
		}
	}
	return -math.Expm1(-hazard)
}

func countNonzero(profile []int) int {
	n := 0
	for _, offset := range profile {
		if offset != 0 {
			n++
		}
	}
	return n
}

func (m *Machine) coreRate(profile []int, spec machine.TrialSpec, core int) float64 {
	return m.coreRateOf(profile, spec.Regime, spec.Workload.ID, core, slices.Contains(spec.Cores, core), m.sharedR7(spec), countNonzero(profile))
}

// coreRateOf is coreRate with the per-trial values precomputed: whether core is loaded, whether the trial is a
// shared-voltage R7 load, and how many offsets in profile are nonzero.
func (m *Machine) coreRateOf(profile []int, regime machine.Regime, workload string, core int, loaded, shared bool, nonzero int) float64 {
	if loaded && shared {
		return 0
	}
	limits := &m.limits[core]
	var limit int
	if loaded {
		alone := nonzero == 0 || (nonzero == 1 && profile[core] != 0)
		limit = m.limit(core, regime, workload, alone)
	} else {
		if limits.Idle == nil && limits.Flat <= 0 {
			return 0
		}
		if limits.Idle != nil {
			limit = *limits.Idle
		}
	}
	rate := limits.Flat
	if loaded || limits.Idle != nil {
		d := limit - profile[core]
		if d >= 1 {
			rate += m.model.PastLimitRate * math.Pow(m.model.Growth, float64(d-1))
		} else if loaded {
			rate += m.model.NearLimitRate
		}
	}
	if profile[core] == 0 {
		rate -= limits.Flat
	}
	return rate
}

func (m *Machine) jointRate(profile []int, regime machine.Regime, j int) float64 {
	joint := &m.cfg.Joints[j]
	if mask := m.jointRegimes[j]; mask != 0 {
		if i := regimeIndex(regime); i < 0 || mask&(1<<i) == 0 {
			return 0
		}
	}
	members := m.joints[j]
	if len(members) == 0 {
		return 0
	}
	for _, member := range members {
		if profile[member.core] > member.offset {
			return 0
		}
	}
	if joint.Rate == 0 {
		return m.model.PastLimitRate
	}
	return joint.Rate
}

type jointMember struct{ core, offset int }

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
	for j := range m.cfg.Joints {
		if m.jointRate(profile, spec.Regime, j) <= 0 {
			continue
		}
		for _, member := range m.joints[j] {
			if member.core/size == ccd {
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

type workloadLimit struct {
	id    string
	limit int
}
