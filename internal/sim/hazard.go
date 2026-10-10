package sim

import (
	"errors"
	"fmt"
	"math"
	"slices"

	"github.com/shgew/togi/internal/machine"
)

// hazards is the part of a machine that sets its failure rates. Machine embeds it; a Predictor is just this.
type hazards struct {
	cfg            Config
	model          Model
	limits         []Limits
	joints         [][]jointMember
	jointRegimes   []uint8
	workloadLimits [][]workloadLimit
}

// index precomputes the joint members, joint regimes and workload limits from joints and h.limits.
func (h *hazards) index(joints []Joint) {
	workloads, members := 0, 0
	for _, limits := range h.limits {
		workloads += len(limits.Workload)
	}
	for _, joint := range joints {
		members += len(joint.Members)
	}
	workloadBacking := make([]workloadLimit, 0, workloads)
	memberBacking := make([]jointMember, 0, members)
	h.workloadLimits = make([][]workloadLimit, len(h.limits))
	for core, limits := range h.limits {
		start := len(workloadBacking)
		for id, limit := range limits.Workload {
			workloadBacking = append(workloadBacking, workloadLimit{id: id, limit: limit})
		}
		h.workloadLimits[core] = workloadBacking[start:len(workloadBacking):len(workloadBacking)]
	}
	h.joints = make([][]jointMember, len(joints))
	h.jointRegimes = make([]uint8, len(joints))
	for j, joint := range joints {
		start := len(memberBacking)
		for core, offset := range joint.Members {
			memberBacking = append(memberBacking, jointMember{core: core, offset: offset})
		}
		h.joints[j] = memberBacking[start:len(memberBacking):len(memberBacking)]
		for _, regime := range joint.Regimes {
			h.jointRegimes[j] |= 1 << regimeIndex(regime)
		}
	}
}

// Predictor reports Hazard and FailureProbability exactly as a Machine built from the same Config would, without the
// simulated host. It costs far less to build, for callers such as the fitter that evaluate many configurations.
type Predictor struct{ hazards }

// NewPredictor builds a predictor from its own copy of the failure rates in cfg, which must give Limits explicitly:
// later changes to cfg never reach it.
func NewPredictor(cfg Config) (*Predictor, error) {
	if cfg.Cores == 0 {
		cfg.Cores = 16
	}
	if err := validateCores(cfg.Cores); err != nil {
		return nil, fmt.Errorf("new predictor: %w", err)
	}
	if cfg.Limits == nil {
		return nil, errors.New("new predictor: limits are required")
	}
	// Joint members are checked on the indexed copy below, which avoids walking each map twice.
	unmembered := slices.Clone(cfg.Joints)
	for j := range unmembered {
		unmembered[j].Members = nil
	}
	if err := errors.Join(validateCCD(cfg.CCD), validateLimits(cfg.Limits, cfg.Cores), validateLimitHazards(cfg.Limits), validateJoints(unmembered, cfg.Cores)); err != nil {
		return nil, fmt.Errorf("new predictor: %w", err)
	}
	voltage, err := normalizeVoltage(cfg.SharedVoltage, cfg.Cores)
	if err != nil {
		return nil, fmt.Errorf("new predictor: %w", err)
	}
	var model Model
	if cfg.Model != nil {
		model = *cfg.Model
	} else {
		model = DefaultModel()
	}
	p := &Predictor{hazards{cfg: Config{Cores: cfg.Cores, SharedVoltage: voltage}, model: model, limits: slices.Clone(cfg.Limits)}}
	if cfg.CCD != nil {
		ccd := *cfg.CCD
		p.cfg.CCD = &ccd
	}
	p.cfg.Joints = make([]Joint, len(cfg.Joints))
	for j, joint := range cfg.Joints {
		p.cfg.Joints[j] = Joint{Rate: joint.Rate, AfterS: joint.AfterS}
	}
	p.index(cfg.Joints)
	for _, members := range p.joints {
		for _, member := range members {
			if member.core < 0 || member.core >= cfg.Cores || member.offset < machine.MinOffset || member.offset > machine.MaxOffset {
				return nil, fmt.Errorf("new predictor: joint member core %d offset %d invalid", member.core, member.offset)
			}
		}
	}
	for core := range p.limits {
		if idle := p.limits[core].Idle; idle != nil {
			value := *idle
			p.limits[core].Idle = &value
		}
		p.limits[core].Workload = nil
	}
	return p, nil
}

// Hazard returns steady-state failures per second, without onset boosts or joint delays.
// Profile must contain one register offset for each machine core.
func (m *hazards) Hazard(profile []int, spec machine.TrialSpec) float64 {
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
func (m *hazards) FailureProbability(profile []int, spec machine.TrialSpec) float64 {
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
	regime := regimeIndex(spec.Regime)
	for core := range m.limits {
		var rate float64
		switch limits := &m.limits[core]; {
		case loaded[core]:
			if shared {
				continue
			}
			rate = m.loadedRate(profile, regime, spec.Workload.ID, core, nonzero)
		case limits.Idle == nil && limits.Flat <= 0:
			continue
		default:
			rate = m.unloadedRate(profile, core)
		}
		hazard += rate * always
	}
	if shared {
		for _, rate := range m.voltageState(profile, spec).rates {
			hazard += rate * always
		}
		hazard += m.cfg.SharedVoltage.BackgroundRate * always
	} else {
		for j := range m.cfg.Joints {
			if mask := m.jointRegimes[j]; mask != 0 && (regime < 0 || mask&(1<<regime) == 0) {
				continue
			}
			if rate := m.jointRateIn(profile, j); rate != 0 {
				hazard += rate * exposure(m.cfg.Joints[j].AfterS)
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

func (m *hazards) coreRate(profile []int, spec machine.TrialSpec, core int) float64 {
	return m.coreRateOf(profile, spec.Regime, spec.Workload.ID, core, slices.Contains(spec.Cores, core), m.sharedR7(spec), countNonzero(profile))
}

// coreRateOf is coreRate with the per-trial values precomputed: whether core is loaded, whether the trial is a
// shared-voltage R7 load, and how many offsets in profile are nonzero.
func (m *hazards) coreRateOf(profile []int, regime machine.Regime, workload string, core int, loaded, shared bool, nonzero int) float64 {
	if !loaded {
		return m.unloadedRate(profile, core)
	}
	if shared {
		return 0
	}
	return m.loadedRate(profile, regimeIndex(regime), workload, core, nonzero)
}

func (m *hazards) loadedRate(profile []int, regime int, workload string, core int, nonzero int) float64 {
	limits := &m.limits[core]
	alone := nonzero == 0 || (nonzero == 1 && profile[core] != 0)
	limit := m.limit(core, regime, workload, alone)
	rate := limits.Flat
	if d := limit - profile[core]; d >= 1 {
		rate += m.model.PastLimitRate * math.Pow(m.model.Growth, float64(d-1))
	} else {
		rate += m.model.NearLimitRate
	}
	if profile[core] == 0 {
		rate -= limits.Flat
	}
	return rate
}

func (m *hazards) unloadedRate(profile []int, core int) float64 {
	limits := &m.limits[core]
	if limits.Idle == nil && limits.Flat <= 0 {
		return 0
	}
	rate := limits.Flat
	if limits.Idle != nil {
		if d := *limits.Idle - profile[core]; d >= 1 {
			rate += m.model.PastLimitRate * math.Pow(m.model.Growth, float64(d-1))
		}
	}
	if profile[core] == 0 {
		rate -= limits.Flat
	}
	return rate
}

func (m *hazards) jointRate(profile []int, regime machine.Regime, j int) float64 {
	if mask := m.jointRegimes[j]; mask != 0 {
		if i := regimeIndex(regime); i < 0 || mask&(1<<i) == 0 {
			return 0
		}
	}
	return m.jointRateIn(profile, j)
}

// jointRateIn is jointRate for a joint that applies in the trial's regime.
func (m *hazards) jointRateIn(profile []int, j int) float64 {
	members := m.joints[j]
	if len(members) == 0 {
		return 0
	}
	for _, member := range members {
		if profile[member.core] > member.offset {
			return 0
		}
	}
	if rate := m.cfg.Joints[j].Rate; rate != 0 {
		return rate
	}
	return m.model.PastLimitRate
}

type jointMember struct{ core, offset int }

func (m *hazards) ccdRate(profile []int, spec machine.TrialSpec, ccd int) float64 {
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
