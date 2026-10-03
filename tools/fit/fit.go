package main

import (
	"encoding/json"
	"fmt"
	"maps"
	"math"
	"math/rand/v2"
	"slices"
	"time"

	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/tools/modelcheck"
	"github.com/shgew/togi/tools/trialfacts"
)

type observation struct {
	profile []int
	spec    machine.TrialSpec
	n, k    int
}

type likelihood struct {
	cfg   sim.Config
	m     *sim.Machine
	obs   []observation
	guard *modelcheck.Checker
}

func decisive(records []trialfacts.Record) ([]trialfacts.Record, error) {
	var out []trialfacts.Record
	var context *machine.BIOSContext
	cores := 0
	for _, r := range records {
		if r.Kind != facts.TrialFact || (r.Outcome != journal.OutcomePass && r.Outcome != journal.OutcomeFailure) {
			continue
		}
		if cores == 0 {
			cores, context = len(r.Profile), r.Context
		}
		if cores < 2 || cores%2 != 0 || len(r.Profile) != cores || len(r.Class.Cores) == 0 || r.Class.DurationS <= 0 || !slices.Contains(machine.Regimes, r.Class.Regime) {
			return nil, fmt.Errorf("invalid trial fact %s:%d", r.Session, r.Seq)
		}
		if (context == nil) != (r.Context == nil) || (context != nil && *context != *r.Context) {
			return nil, fmt.Errorf("mixed BIOS contexts: split the extract before fitting")
		}
		for _, offset := range r.Profile {
			if offset < -50 || offset > 0 {
				return nil, fmt.Errorf("invalid profile in fact %s:%d", r.Session, r.Seq)
			}
		}
		for _, core := range r.Class.Cores {
			if core < 0 || core >= cores {
				return nil, fmt.Errorf("invalid loaded core in fact %s:%d", r.Session, r.Seq)
			}
		}
		out = append(out, r)
	}
	if len(out) == 0 {
		return nil, fmt.Errorf("extract has no decisive starts")
	}
	return out, nil
}

func aggregate(records []trialfacts.Record) []observation {
	indices := make(map[string]int)
	var out []observation
	for _, r := range records {
		key, _ := json.Marshal(struct {
			Profile []int
			Class   facts.Class
		}{r.Profile, r.Class})
		i, ok := indices[string(key)]
		if !ok {
			i = len(out)
			indices[string(key)] = i
			out = append(out, observation{profile: r.Profile, spec: machine.TrialSpec{Regime: r.Class.Regime, Workload: machine.Workload{ID: r.Class.Workload}, Cores: r.Class.Cores, Duration: time.Duration(r.Class.DurationS) * time.Second}})
		}
		out[i].n++
		if r.Outcome == journal.OutcomeFailure {
			out[i].k++
		}
	}
	return out
}

func bootstrap(records []trialfacts.Record, seed uint64) []trialfacts.Record {
	rng := rand.New(rand.NewPCG(seed, seed^0x9e3779b97f4a7c15))
	out := make([]trialfacts.Record, len(records))
	for i := range out {
		out[i] = records[rng.IntN(len(records))]
	}
	return out
}

func (l *likelihood) rebuild() {
	m, err := sim.New(l.cfg)
	if err != nil {
		panic(err)
	}
	l.m = m
}

func (l *likelihood) value(i int) float64 {
	o := &l.obs[i]
	p := l.m.FailureProbability(o.profile, o.spec)
	var loss float64
	if o.k > 0 {
		if p <= 0 {
			return math.Inf(1)
		}
		loss -= float64(o.k) * math.Log(p)
	}
	if o.n > o.k {
		if p >= 1 {
			return math.Inf(1)
		}
		loss -= float64(o.n-o.k) * math.Log1p(-p)
	}
	return loss
}

func (l *likelihood) selectObs(include func(observation) bool) []int {
	var indices []int
	for i, o := range l.obs {
		if include(o) {
			indices = append(indices, i)
		}
	}
	return indices
}

func (l *likelihood) score(indices []int) float64 {
	if !l.admissible() {
		return math.Inf(1)
	}
	return l.rawScore(indices)
}

func (l *likelihood) admissible() bool {
	return l.guard == nil || l.guard.Accepts(l.m)
}

func (l *likelihood) rawScore(indices []int) float64 {
	var loss float64
	for _, i := range indices {
		loss += l.value(i)
	}
	if s := l.cfg.SingleCore; s != nil {
		for _, effect := range s.Core {
			loss += 0.125 * effect * effect
		}
		for _, effect := range s.Workload {
			loss += 0.5 * effect * effect
		}
	}
	return loss
}

func (l *likelihood) discrete(dst *int, indices []int) {
	if len(indices) == 0 {
		return
	}
	best, score := *dst, l.score(indices)
	for edge := -50; edge <= 1; edge++ {
		*dst = edge
		candidate := l.rawScore(indices)
		if candidate < score-1e-9 && l.admissible() {
			best, score = edge, candidate
		}
	}
	*dst = best
}

func (l *likelihood) continuous(get func() float64, set func(float64), indices []int, low, high float64) {
	if len(indices) == 0 {
		return
	}
	best, score := get(), l.score(indices)
	evaluate := func(x float64) float64 {
		set(x)
		v := l.score(indices)
		if v < score-1e-9 {
			best, score = x, v
		}
		return v
	}
	evaluate(0)
	const ratio = 0.6180339887498949
	a, b := math.Log(low), math.Log(high)
	x, y := b-ratio*(b-a), a+ratio*(b-a)
	fx, fy := evaluate(math.Exp(x)), evaluate(math.Exp(y))
	for range 24 {
		if fx < fy {
			b, y, fy = y, x, fx
			x = b - ratio*(b-a)
			fx = evaluate(math.Exp(x))
		} else {
			a, x, fx = x, y, fy
			y = a + ratio*(b-a)
			fy = evaluate(math.Exp(y))
		}
	}
	set(best)
}

func initialConfig(records []trialfacts.Record) sim.Config {
	cores := len(records[0].Profile)
	model := sim.DefaultModel()
	model.PastEdgeRate, model.Growth, model.NearEdgeRate = 0.015, 2, 1e-7
	cfg := sim.Config{Cores: cores, Edges: make([]sim.Edges, cores), Model: &model}
	cfg.SingleCore = &sim.SingleCore{LogRate: -9, Slope: 0.15, Core: make([]float64, cores), Workload: make(map[string]float64)}
	for _, r := range records {
		if len(r.Class.Cores) == 1 && (r.Class.Regime == machine.R1 || r.Class.Regime == machine.R2) {
			cfg.SingleCore.Workload[r.Class.Workload] = 0
		}
	}
	if records[0].Context != nil {
		cfg.BIOSContext = *records[0].Context
	}
	for core := range cfg.Edges {
		for r := range cfg.Edges[core].Isolated {
			cfg.Edges[core].Isolated[r] = -50
		}
		for r := range cfg.Edges[core].Resident {
			cfg.Edges[core].Resident[r] = -50
		}
	}
	for ccd := range 2 {
		members := make(map[int]int)
		for core := ccd * cores / 2; core < (ccd+1)*cores/2; core++ {
			members[core] = -50
		}
		found := false
		for _, r := range records {
			if r.Class.Regime != machine.R7 || r.Outcome != journal.OutcomeFailure {
				continue
			}
			all := true
			for core := range members {
				all = all && r.Profile[core] < 0
			}
			if !all {
				continue
			}
			found = true
			for core := range members {
				members[core] = max(members[core], r.Profile[core])
			}
		}
		if found {
			cfg.Joints = append(cfg.Joints, sim.Joint{Members: members, Regimes: []machine.Regime{machine.R7}, Rate: 0.005, Signal: machine.Crash})
		}
	}
	return cfg
}

func fit(records []trialfacts.Record) (sim.Config, float64) {
	return fitFrom(records, nil, nil)
}

func fitFrom(records []trialfacts.Record, initial *sim.Config, guard *modelcheck.Checker) (sim.Config, float64) {
	var cfg sim.Config
	if initial == nil {
		cfg = initialConfig(records)
	} else {
		cfg = cloneMachine(*initial)
	}
	l := likelihood{cfg: cfg, obs: aggregate(records), guard: guard}
	l.rebuild()
	all := l.selectObs(func(observation) bool { return true })
	previous := math.Inf(1)
	for range 12 {
		l.fitSingleCore(&cfg)
		l.fitJoints(&cfg)
		for ccd := range 2 {
			l.addJoint(&cfg, records, ccd)
		}
		l.fitRegimeEdges(&cfg)
		l.fitWorkloads(&cfg)
		l.fitHazardShape(cfg.Model, all)
		l.fitFlat(&cfg)
		l.fitIdle(&cfg)
		l.fitEdgeRateShift(&cfg, all)
		score := l.score(all)
		if previous-score < 1e-5 {
			break
		}
		previous = score
	}
	return cfg, l.score(all)
}

func (l *likelihood) fitJoints(cfg *sim.Config) {
	for j := range cfg.Joints {
		joint := &cfg.Joints[j]
		indices := l.selectObs(func(o observation) bool { return slices.Contains(joint.Regimes, o.spec.Regime) })
		l.continuous(func() float64 { return joint.Rate }, func(x float64) { joint.Rate = max(x, 1e-12) }, indices, 1e-7, 0.5)
		for core := range cfg.Cores {
			edge, ok := joint.Members[core]
			if !ok {
				continue
			}
			best, score := edge, l.score(indices)
			for candidate := -50; candidate <= 0; candidate++ {
				joint.Members[core] = candidate
				v := l.rawScore(indices)
				if v < score-1e-9 && l.admissible() {
					best, score = candidate, v
				}
			}
			joint.Members[core] = best
		}
	}
}

func (l *likelihood) fitRegimeEdges(cfg *sim.Config) {
	for core := range cfg.Edges {
		for r, regime := range machine.Regimes {
			for _, isolated := range []bool{true, false} {
				if isolated && r >= 5 {
					continue
				}
				indices := l.selectObs(func(o observation) bool {
					if o.spec.Regime != regime || !slices.Contains(o.spec.Cores, core) {
						return false
					}
					if cfg.SingleCore != nil && singleCoreObservation(o) {
						return false
					}
					only := r < 5
					for c, offset := range o.profile {
						if c != core && offset != 0 {
							only = false
						}
					}
					return only == isolated
				})
				if isolated {
					l.discrete(&cfg.Edges[core].Isolated[r], indices)
				} else {
					l.discrete(&cfg.Edges[core].Resident[r], indices)
				}
			}
		}
	}
}

func (l *likelihood) fitHazardShape(model *sim.Model, all []int) {
	for _, dst := range []*float64{&model.PastEdgeRate, &model.Growth, &model.NearEdgeRate} {
		low, high := 1e-8, 0.5
		if dst == &model.Growth {
			low, high = 1.05, 8
		} else if dst == &model.NearEdgeRate {
			high = 0.001
		}
		l.continuous(func() float64 { return *dst }, func(x float64) {
			if dst == &model.Growth {
				x = max(x, 1.05)
			}
			*dst = x
			l.rebuild()
		}, all, low, high)
	}
}

func (l *likelihood) fitFlat(cfg *sim.Config) {
	for core := range cfg.Edges {
		indices := l.selectObs(func(o observation) bool { return o.profile[core] < 0 })
		dst := &cfg.Edges[core].Flat
		l.continuous(func() float64 { return *dst }, func(x float64) { *dst = x }, indices, 1e-10, 0.001)
	}
}

func (l *likelihood) fitIdle(cfg *sim.Config) {
	for core := range cfg.Edges {
		hasExposure := false
		indices := l.selectObs(func(o observation) bool {
			if slices.Contains(o.spec.Cores, core) {
				return false
			}
			hasExposure = hasExposure || o.profile[core] < 0
			return true
		})
		idle := -50
		if cfg.Edges[core].Idle != nil {
			idle = *cfg.Edges[core].Idle
		}
		cfg.Edges[core].Idle = &idle
		// Zero-offset starts constrain Idle=1, but cannot alone identify an idle edge.
		if hasExposure {
			l.discrete(&idle, indices)
		}
		if idle == -50 {
			cfg.Edges[core].Idle = nil
		}
	}
}

func (l *likelihood) addJoint(cfg *sim.Config, records []trialfacts.Record, ccd int) {
	count := 0
	for _, joint := range cfg.Joints {
		if _, ok := joint.Members[ccd*cfg.Cores/2]; ok {
			count++
		}
	}
	if count >= 8 {
		return
	}
	indices := l.selectObs(func(o observation) bool { return o.spec.Regime == machine.R7 })
	baseline := l.score(indices)
	bestScore := baseline - 0.5
	var best sim.Joint
	seen := make(map[string]bool)
	original := len(cfg.Joints)
	cfg.Joints = append(cfg.Joints, sim.Joint{Regimes: []machine.Regime{machine.R7}, Rate: 0.001, Signal: machine.Crash})
	l.cfg = *cfg
	l.rebuild()
	candidate := &cfg.Joints[original]
	for _, r := range records {
		if r.Class.Regime != machine.R7 || r.Outcome != journal.OutcomeFailure {
			continue
		}
		members := make(map[int]int)
		for core := ccd * cfg.Cores / 2; core < (ccd+1)*cfg.Cores/2; core++ {
			if r.Profile[core] == 0 {
				break
			}
			members[core] = r.Profile[core]
		}
		if len(members) != cfg.Cores/2 {
			continue
		}
		key, _ := json.Marshal(members)
		if seen[string(key)] {
			continue
		}
		seen[string(key)] = true
		duplicate := false
		for _, joint := range cfg.Joints[:original] {
			duplicate = duplicate || maps.Equal(joint.Members, members)
		}
		if duplicate {
			continue
		}
		candidate.Members, candidate.Rate = members, 0.001
		l.continuous(func() float64 { return candidate.Rate }, func(x float64) { candidate.Rate = max(x, 1e-12) }, indices, 1e-7, 0.5)
		score := l.score(indices)
		if score < bestScore {
			best, bestScore = *candidate, score
		}
	}
	if best.Members == nil {
		cfg.Joints = cfg.Joints[:original]
	} else {
		cfg.Joints[original] = best
	}
	l.cfg = *cfg
	l.rebuild()
}

func (l *likelihood) fitWorkloads(cfg *sim.Config) {
	for core := range cfg.Edges {
		workloads := make(map[string]bool)
		for _, o := range l.obs {
			if o.k > 0 && slices.Contains(o.spec.Cores, core) {
				if cfg.SingleCore != nil && singleCoreObservation(o) {
					continue
				}
				workloads[o.spec.Workload.ID] = true
			}
		}
		edge := &cfg.Edges[core]
		for _, workload := range slices.Sorted(maps.Keys(workloads)) {
			if workload == "" {
				continue
			}
			indices := l.selectObs(func(o observation) bool { return o.spec.Workload.ID == workload && slices.Contains(o.spec.Cores, core) })
			starts := 0
			for _, i := range indices {
				starts += l.obs[i].n
			}
			if starts < 10 {
				continue
			}
			baseline := l.score(indices)
			value, exists := edge.Workload[workload]
			if edge.Workload == nil {
				edge.Workload = make(map[string]int)
			}
			if !exists {
				value = -50
			}
			best, score := value, math.Inf(1)
			for candidate := -50; candidate <= 1; candidate++ {
				edge.Workload[workload] = candidate
				loss := l.rawScore(indices)
				if loss < score-1e-9 && l.admissible() {
					best, score = candidate, loss
				}
			}
			if !exists && baseline-score < 0.5 {
				delete(edge.Workload, workload)
			} else {
				edge.Workload[workload] = best
			}
		}
		if len(edge.Workload) == 0 {
			edge.Workload = nil
		}
	}
}

func cloneMachine(cfg sim.Config) sim.Config {
	model := *cfg.Model
	cfg.Model = &model
	if cfg.SingleCore != nil {
		s := *cfg.SingleCore
		s.Core = slices.Clone(s.Core)
		s.Workload = maps.Clone(s.Workload)
		cfg.SingleCore = &s
	}
	cfg.Edges = slices.Clone(cfg.Edges)
	for core := range cfg.Edges {
		cfg.Edges[core].Workload = maps.Clone(cfg.Edges[core].Workload)
		if cfg.Edges[core].Idle != nil {
			value := *cfg.Edges[core].Idle
			cfg.Edges[core].Idle = &value
		}
	}
	cfg.Joints = slices.Clone(cfg.Joints)
	for j := range cfg.Joints {
		cfg.Joints[j].Members = maps.Clone(cfg.Joints[j].Members)
	}
	return cfg
}

type edgeShift struct {
	value    int
	dst      *int
	workload map[string]int
	key      string
}

func edgeShifts(cfg *sim.Config) []edgeShift {
	var shifts []edgeShift
	add := func(dst *int) {
		if *dst > -50 {
			shifts = append(shifts, edgeShift{value: *dst, dst: dst})
		}
	}
	for core := range cfg.Edges {
		edge := &cfg.Edges[core]
		for r := range edge.Isolated {
			add(&edge.Isolated[r])
		}
		for r := range edge.Resident {
			add(&edge.Resident[r])
		}
		if edge.Idle != nil {
			add(edge.Idle)
		}
		for _, key := range slices.Sorted(maps.Keys(edge.Workload)) {
			if value := edge.Workload[key]; value > -50 {
				shifts = append(shifts, edgeShift{value: value, workload: edge.Workload, key: key})
			}
		}
	}
	return shifts
}

func (l *likelihood) fitEdgeRateShift(cfg *sim.Config, all []int) {
	shifts := edgeShifts(cfg)
	if len(shifts) == 0 || cfg.Model.PastEdgeRate == 0 {
		return
	}
	low, high := -50, 50
	for _, edge := range shifts {
		low, high = max(low, -50-edge.value), min(high, 1-edge.value)
	}
	rate := cfg.Model.PastEdgeRate
	apply := func(delta int, shiftedRate float64) {
		for _, edge := range shifts {
			if edge.dst != nil {
				*edge.dst = edge.value + delta
			} else {
				edge.workload[edge.key] = edge.value + delta
			}
		}
		cfg.Model.PastEdgeRate = shiftedRate
		l.rebuild()
	}
	best, bestRate, score := 0, rate, l.score(all)
	for delta := low; delta <= high; delta++ {
		shiftedRate := rate * math.Pow(cfg.Model.Growth, float64(-delta))
		if shiftedRate < 1e-8 || shiftedRate > 0.5 {
			continue
		}
		apply(delta, shiftedRate)
		candidate := l.rawScore(all)
		if candidate < score-1e-9 && l.admissible() {
			best, bestRate, score = delta, shiftedRate, candidate
		}
	}
	apply(best, bestRate)
}
