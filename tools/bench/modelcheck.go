package main

import (
	"encoding/json"
	"fmt"
	"io"
	"math"
	"path/filepath"
	"slices"
	"time"

	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/tools/trialfacts"
)

type checkedGroup struct {
	Context  *machine.BIOSContext `json:"context"`
	Kind     facts.Kind           `json:"kind"`
	Class    facts.Class          `json:"class"`
	Depth    int                  `json:"depth"`
	N        int                  `json:"n"`
	K        int                  `json:"k"`
	Interval [2]int               `json:"interval"`
	MeanP    float64              `json:"mean_p"`
	Flagged  bool                 `json:"flagged"`
}

type modelCheck struct {
	Machine      string         `json:"machine"`
	Extract      string         `json:"extract"`
	Status       string         `json:"status"`
	Groups       []checkedGroup `json:"groups"`
	IdleFailures int            `json:"idle_failures"`
}

func groupOf(r trialfacts.Record) checkedGroup {
	depth := 0
	if len(r.Class.Cores) > 0 {
		depth = r.Profile[r.Class.Cores[0]]
		for _, core := range r.Class.Cores {
			depth = max(depth, r.Profile[core])
		}
	}
	return checkedGroup{Context: r.Context, Kind: r.Kind, Class: r.Class, Depth: depth}
}

func checkModel(path string, cfg sim.Config) (*modelCheck, error) {
	if cfg.Model != nil {
		for _, kind := range []machine.ResetKind{machine.ResetThermalTrip, machine.ResetPowerLoss} {
			if cfg.Model.Reset[kind] > 0 {
				return nil, fmt.Errorf("model check requires decisive failures: reset %q has positive weight", kind)
			}
		}
	}
	extract := cfg.Facts
	if !filepath.IsAbs(extract) {
		extract = filepath.Join(filepath.Dir(path), extract)
	}
	records, err := trialfacts.Read(extract)
	if err != nil {
		return nil, err
	}
	m, err := sim.New(cfg)
	if err != nil {
		return nil, err
	}
	cores := cfg.Cores
	if cores == 0 {
		cores = 16
	}
	check := &modelCheck{Machine: path, Extract: extract, Status: "ok", Groups: []checkedGroup{}}
	groups := map[string]*checkedGroup{}
	for _, r := range records {
		if cfg.BIOSContext != (machine.BIOSContext{}) && (r.Context == nil || *r.Context != cfg.BIOSContext) {
			continue
		}
		if r.Kind == facts.IdleFact {
			check.IdleFailures++
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
		key, err := json.Marshal(group)
		if err != nil {
			return nil, fmt.Errorf("encode group: %w", err)
		}
		g := groups[string(key)]
		if g == nil {
			g = &group
			groups[string(key)] = g
		}
		spec := machine.TrialSpec{Regime: r.Class.Regime, Workload: machine.Workload{ID: r.Class.Workload}, Cores: r.Class.Cores, Duration: time.Duration(r.Class.DurationS) * time.Second, Condition: r.Condition}
		g.N++
		if r.Outcome == journal.OutcomeFailure {
			g.K++
		}
		g.MeanP += m.FailureProbability(r.Profile, spec)
	}
	keys := make([]string, 0, len(groups))
	for key := range groups {
		keys = append(keys, key)
	}
	slices.Sort(keys)
	for _, key := range keys {
		g := groups[key]
		if g.N < 10 {
			continue
		}
		g.MeanP /= float64(g.N)
		g.Interval = binomialInterval(g.N, g.MeanP)
		g.Flagged = g.K < g.Interval[0] || g.K > g.Interval[1]
		if g.Flagged {
			check.Status = "flagged"
		}
		check.Groups = append(check.Groups, *g)
	}
	return check, nil
}

func binomialInterval(n int, p float64) [2]int {
	if p <= 0 {
		return [2]int{0, 0}
	}
	if p >= 1 {
		return [2]int{n, n}
	}
	logN, _ := math.Lgamma(float64(n + 1))
	weights := make([]float64, n+1)
	largest := math.Inf(-1)
	for k := range weights {
		a, _ := math.Lgamma(float64(k + 1))
		b, _ := math.Lgamma(float64(n - k + 1))
		weights[k] = logN - a - b + float64(k)*math.Log(p) + float64(n-k)*math.Log1p(-p)
		largest = max(largest, weights[k])
	}
	var total float64
	for k := range weights {
		weights[k] = math.Exp(weights[k] - largest)
		total += weights[k]
	}
	interval := [2]int{n, n}
	var cumulative float64
	lowerFound := false
	for k, weight := range weights {
		cumulative += weight
		if !lowerFound && cumulative >= 0.005*total {
			interval[0] = k
			lowerFound = true
		}
		if cumulative >= 0.995*total {
			interval[1] = k
			break
		}
	}
	return interval
}

func reportModelChecks(w io.Writer, checks []*modelCheck) {
	if len(checks) == 0 {
		return
	}
	fmt.Fprintln(w, "\nModel check (99% binomial intervals)")
	for _, check := range checks {
		fmt.Fprintf(w, "%s: %s (%d eligible groups; %d idle failures without start exposure)\n", check.Machine, check.Status, len(check.Groups), check.IdleFailures)
		for _, g := range check.Groups {
			if g.Flagged {
				fmt.Fprintf(w, "  %s %s cores=%v duration=%ds depth=%d n=%d k=%d interval=[%d,%d] mean_p=%.6g\n", g.Class.Regime, g.Class.Workload, g.Class.Cores, g.Class.DurationS, g.Depth, g.N, g.K, g.Interval[0], g.Interval[1], g.MeanP)
			}
		}
	}
}
