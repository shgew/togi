package modelcheck

import (
	"fmt"
	"io"
	"math"

	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/tools/trialfacts"
)

type Group struct {
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

type Result struct {
	Machine      string  `json:"machine"`
	Extract      string  `json:"extract"`
	Status       string  `json:"status"`
	Groups       []Group `json:"groups"`
	IdleFailures int     `json:"idle_failures"`
}

func groupOf(r trialfacts.Record) Group {
	depth := 0
	if len(r.Class.Cores) > 0 {
		depth = r.Profile[r.Class.Cores[0]]
		for _, core := range r.Class.Cores {
			depth = max(depth, r.Profile[core])
		}
	}
	return Group{Context: r.Context, Kind: r.Kind, Class: r.Class, Depth: depth}
}

func Check(path string, cfg sim.Config, extracts trialfacts.Extracts) (*Result, error) {
	extract, records, err := extracts.Load(path, cfg)
	if err != nil {
		return nil, err
	}
	return CheckRecords(path, extract, cfg, records)
}

func CheckRecords(path, extract string, cfg sim.Config, records []trialfacts.Record) (*Result, error) {
	m, err := sim.New(cfg)
	if err != nil {
		return nil, err
	}
	checker, err := NewChecker(cfg, records)
	if err != nil {
		return nil, err
	}
	return checker.Check(path, extract, m), nil
}

func binomialInterval(n int, p float64) [2]int {
	return intervalWithWeights(n, p, make([]float64, n+1))
}

func intervalWithWeights(n int, p float64, weights []float64) [2]int {
	if p <= 0 {
		return [2]int{0, 0}
	}
	if p >= 1 {
		return [2]int{n, n}
	}
	logN, _ := math.Lgamma(float64(n + 1))
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

func Report(w io.Writer, checks []*Result) {
	if len(checks) == 0 {
		return
	}
	fmt.Fprintln(w, "\nModel check (99% binomial intervals)")
	for _, check := range checks {
		fmt.Fprintf(w, "%s: %s (%d eligible groups; %d idle failures without trial exposure)\n", check.Machine, check.Status, len(check.Groups), check.IdleFailures)
		for _, g := range check.Groups {
			if g.Flagged {
				fmt.Fprintf(w, "  %s %s cores=%v duration=%ds depth=%d n=%d k=%d interval=[%d,%d] mean_p=%.6g\n", g.Class.Regime, g.Class.Workload, g.Class.Cores, g.Class.DurationS, g.Depth, g.N, g.K, g.Interval[0], g.Interval[1], g.MeanP)
			}
		}
	}
}
