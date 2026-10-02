package modelcheck

import (
	"encoding/json"
	"fmt"
	"slices"
	"time"

	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/tools/trialfacts"
)

type point struct {
	profile []int
	spec    machine.TrialSpec
	n       int
}

type preparedGroup struct {
	group  Group
	points []point
	bounds [2]float64
}

type Checker struct {
	groups []preparedGroup
	idle   int
}

func NewChecker(cfg sim.Config, records []trialfacts.Record) (*Checker, error) {
	if cfg.Model != nil {
		for _, kind := range []machine.ResetKind{machine.ResetThermalTrip, machine.ResetPowerLoss} {
			if cfg.Model.Reset[kind] > 0 {
				return nil, fmt.Errorf("model check requires decisive failures: reset %q has positive weight", kind)
			}
		}
	}
	cores := cfg.Cores
	if cores == 0 {
		cores = 16
	}
	checker := &Checker{}
	groups := make(map[string]*preparedGroup)
	points := make(map[string]map[string]int)
	for _, r := range records {
		if cfg.BIOSContext != (machine.BIOSContext{}) && (r.Context == nil || *r.Context != cfg.BIOSContext) {
			continue
		}
		if r.Kind == facts.IdleFact {
			checker.idle++
			continue
		}
		if r.Kind != facts.TrialFact || (r.Outcome != journal.OutcomePass && r.Outcome != journal.OutcomeFailure) {
			continue
		}
		if len(r.Profile) != cores || len(r.Class.Cores) == 0 || r.Class.DurationS <= 0 {
			return nil, fmt.Errorf("invalid trial fact %s:%d", r.Session, r.Seq)
		}
		for _, core := range r.Class.Cores {
			if core < 0 || core >= cores {
				return nil, fmt.Errorf("invalid loaded core in fact %s:%d", r.Session, r.Seq)
			}
		}
		group := groupOf(r)
		encoded, err := json.Marshal(group)
		if err != nil {
			return nil, fmt.Errorf("encode group: %w", err)
		}
		key := string(encoded)
		g := groups[key]
		if g == nil {
			g = &preparedGroup{group: group}
			groups[key] = g
			points[key] = make(map[string]int)
		}
		g.group.N++
		if r.Outcome == journal.OutcomeFailure {
			g.group.K++
		}
		profile, _ := json.Marshal(r.Profile)
		index, ok := points[key][string(profile)]
		if !ok {
			index = len(g.points)
			points[key][string(profile)] = index
			g.points = append(g.points, point{profile: r.Profile, spec: machine.TrialSpec{Regime: r.Class.Regime, Workload: machine.Workload{ID: r.Class.Workload}, Cores: r.Class.Cores, Duration: time.Duration(r.Class.DurationS) * time.Second, Condition: r.Condition}})
		}
		g.points[index].n++
	}
	keys := make([]string, 0, len(groups))
	for key := range groups {
		keys = append(keys, key)
	}
	slices.Sort(keys)
	for _, key := range keys {
		if g := groups[key]; g.group.N >= 10 {
			g.bounds = probabilityBounds(g.group.N, g.group.K)
			checker.groups = append(checker.groups, *g)
		}
	}
	return checker, nil
}

func probabilityBounds(n, k int) [2]float64 {
	weights := make([]float64, n+1)
	bounds := [2]float64{0, 1}
	if k > 0 {
		low, high := 0.0, 1.0
		for range 52 {
			mid := (low + high) / 2
			if intervalWithWeights(n, mid, weights)[1] < k {
				low = mid
			} else {
				high = mid
			}
		}
		bounds[0] = high
	}
	if k < n {
		low, high := 0.0, 1.0
		for range 52 {
			mid := (low + high) / 2
			if intervalWithWeights(n, mid, weights)[0] > k {
				high = mid
			} else {
				low = mid
			}
		}
		bounds[1] = low
	}
	return bounds
}

func (g *preparedGroup) meanP(m *sim.Machine) float64 {
	var total float64
	for _, p := range g.points {
		total += float64(p.n) * m.FailureProbability(p.profile, p.spec)
	}
	return total / float64(g.group.N)
}

func (c *Checker) Accepts(m *sim.Machine) bool {
	for i := range c.groups {
		g := &c.groups[i]
		p := g.meanP(m)
		if p < g.bounds[0] || p > g.bounds[1] {
			return false
		}
	}
	return true
}

func (c *Checker) Check(path, extract string, m *sim.Machine) *Result {
	check := &Result{Machine: path, Extract: extract, Status: "ok", Groups: []Group{}, IdleFailures: c.idle}
	for i := range c.groups {
		g := c.groups[i].group
		g.MeanP = c.groups[i].meanP(m)
		g.Interval = binomialInterval(g.N, g.MeanP)
		g.Flagged = g.K < g.Interval[0] || g.K > g.Interval[1]
		if g.Flagged {
			check.Status = "flagged"
		}
		check.Groups = append(check.Groups, g)
	}
	return check
}
