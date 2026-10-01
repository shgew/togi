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
	for core := range m.edges {
		rate += m.coreRate(profile, spec, core)
	}
	for _, joint := range m.cfg.Joints {
		rate += m.jointRate(profile, spec.Regime, joint)
	}
	return rate
}

func (m *Machine) coreRate(profile []int, spec machine.TrialSpec, core int) float64 {
	loaded := slices.Contains(spec.Cores, core)
	edge := m.edge(profile, core, spec.Regime, spec.Workload.ID)
	if !loaded {
		if m.edges[core].Idle == nil && m.edges[core].Flat <= 0 {
			return 0
		}
		if m.edges[core].Idle != nil {
			edge = *m.edges[core].Idle
		}
	}
	rate := m.edges[core].Flat
	if loaded || m.edges[core].Idle != nil {
		d := edge - profile[core]
		if d >= 1 {
			rate += m.model.PastEdgeRate * math.Pow(m.model.Growth, float64(d-1))
		} else if loaded {
			rate += m.model.NearEdgeRate
		}
	}
	if profile[core] == 0 {
		rate -= m.edges[core].Flat
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
		return m.model.PastEdgeRate
	}
	return joint.Rate
}
