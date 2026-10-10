package main

import (
	"errors"
	"fmt"
	"io"
	"math"
	"math/rand/v2"
	"slices"
	"strings"
)

// gate is a ruleset's bench gate, registered in the suite file before it is scored.
type gate struct {
	ID            string       `toml:"id"`
	Note          string       `toml:"note"`
	Baseline      gateBaseline `toml:"baseline"`
	Split         string       `toml:"split"`
	Gated         []string     `toml:"gated"`
	Pooled        []string     `toml:"pooled"`
	Conclude      []string     `toml:"conclude"`
	Quantiles     []float64    `toml:"quantiles"`
	Confidence    float64      `toml:"confidence"`
	Resamples     int          `toml:"resamples"`
	BootstrapSeed [2]uint64    `toml:"bootstrap_seed"`
	MaxTimeRatio  float64      `toml:"max_time_ratio"`

	// seeds holds each gated scenario's seeds in the gate's split, in suite order.
	seeds map[string][]uint64
}

// gateBaseline names the recorded runs a gate is judged against: every baseline run must carry
// the ruleset and one of the commits, so a pull request that re-records some runs lists its
// recording commit here.
type gateBaseline struct {
	Ruleset int      `toml:"ruleset"`
	Commits []string `toml:"commits"`
}

var errGateFailed = errors.New("gate failed")

// resolve checks the gate against the suite's scenarios and records the seeds it requires.
func (g *gate) resolve(scenarios []scenario) error {
	if g.ID == "" || g.Baseline.Ruleset <= 0 || len(g.Baseline.Commits) == 0 || slices.Contains(g.Baseline.Commits, "") {
		return errors.New("gate needs an id and a baseline ruleset and commits")
	}
	if g.Split != "dev" && g.Split != "holdout" && g.Split != "all" {
		return fmt.Errorf("gate %s: split must be dev, holdout or all", g.ID)
	}
	if len(g.Gated) == 0 || len(g.Quantiles) == 0 {
		return fmt.Errorf("gate %s: needs gated scenarios and quantiles", g.ID)
	}
	for _, name := range g.Pooled {
		if !slices.Contains(g.Gated, name) {
			return fmt.Errorf("gate %s: pooled scenario %q is not gated", g.ID, name)
		}
	}
	for _, q := range g.Quantiles {
		if !(q > 0 && q < 1) {
			return fmt.Errorf("gate %s: quantile %g outside (0, 1)", g.ID, q)
		}
	}
	if !(g.Confidence > 0 && g.Confidence < 1) || g.Resamples <= 0 || !(g.MaxTimeRatio > 0) {
		return fmt.Errorf("gate %s: needs confidence in (0, 1), positive resamples and positive max_time_ratio", g.ID)
	}
	byName := make(map[string]scenario, len(scenarios))
	for _, s := range scenarios {
		byName[s.Name] = s
	}
	g.seeds = make(map[string][]uint64)
	for _, name := range slices.Concat(g.Gated, g.Conclude) {
		s, ok := byName[name]
		if !ok {
			return fmt.Errorf("gate %s: unknown scenario %q", g.ID, name)
		}
		var seeds []uint64
		if g.Split != "holdout" {
			seeds = append(seeds, s.Dev...)
		}
		if g.Split != "dev" {
			seeds = append(seeds, s.Holdout...)
		}
		if len(seeds) == 0 {
			return fmt.Errorf("gate %s: scenario %s has no %s seeds", g.ID, name, g.Split)
		}
		g.seeds[name] = seeds
	}
	return nil
}

// criterion is one judged gate criterion.
type criterion struct {
	Kind      string // conclusion, a quantile label, time or pooled_median
	Scenario  string
	Baseline  float64
	Candidate float64
	Change    float64
	Lo, Hi    float64 // the bootstrap interval of Change, for quantiles
	Lost      []uint64
	Timed     int
	Pass      bool
}

// gateResult is a gate's verdict; Refused names why it could not be judged.
type gateResult struct {
	Refused  string
	Criteria []criterion
}

func (r gateResult) pass() bool {
	return r.Refused == "" && !slices.ContainsFunc(r.Criteria, func(c criterion) bool { return !c.Pass })
}

// pairGateRuns checks baseline identity and pairs every required seed with the metrics the gate needs.
func pairGateRuns(g *gate, candidate, baseline []result) (map[string][]pair, error) {
	for _, r := range baseline {
		if r.Ruleset != g.Baseline.Ruleset || !slices.Contains(g.Baseline.Commits, r.Commit) {
			return nil, fmt.Errorf("baseline run %s/%d is ruleset %d at %s, not ruleset %d at %s", r.Scenario, r.Seed, r.Ruleset, r.Commit, g.Baseline.Ruleset, strings.Join(g.Baseline.Commits, " or "))
		}
	}
	index := func(results []result) map[key]result {
		m := make(map[key]result, len(results))
		for _, r := range results {
			m[key{r.Scenario, r.Seed}] = r
		}
		return m
	}
	cand, base := index(candidate), index(baseline)
	pairs := make(map[string][]pair)
	for _, name := range slices.Concat(g.Gated, g.Conclude) {
		if _, done := pairs[name]; done {
			continue
		}
		for _, seed := range g.seeds[name] {
			k := key{name, seed}
			a, aok := cand[k]
			b, bok := base[k]
			var missing []string
			if !aok {
				missing = append(missing, "candidate")
			}
			if !bok {
				missing = append(missing, "baseline")
			}
			if len(missing) > 0 {
				return nil, fmt.Errorf("incomplete pairing: %s seed %d has no %s run; the gate needs every %s seed of its scenarios", name, seed, strings.Join(missing, " or "), g.Split)
			}
			pairs[name] = append(pairs[name], pair{a, b})
		}
	}
	for _, name := range g.Gated {
		for _, p := range pairs[name] {
			if p.candidate.WorstR7HazardPerH == nil || p.baseline.WorstR7HazardPerH == nil {
				return nil, fmt.Errorf("%s seed %d lacks worst_r7_hazard_per_h on a run", name, p.candidate.Seed)
			}
		}
	}
	return pairs, nil
}

// judgeGate scores candidate against baseline under g. It refuses unless the baseline is the
// one g names and every required seed is paired with the metrics its criteria need.
func judgeGate(g *gate, candidate, baseline []result) gateResult {
	pairs, err := pairGateRuns(g, candidate, baseline)
	if err != nil {
		return gateResult{Refused: err.Error()}
	}

	var out gateResult
	for _, name := range slices.Concat(g.Gated, g.Conclude) {
		c := criterion{Kind: "conclusion", Scenario: name}
		for _, p := range pairs[name] {
			if p.baseline.Status == "concluded" {
				c.Baseline++
				if p.candidate.Status != "concluded" {
					c.Lost = append(c.Lost, p.candidate.Seed)
				}
			}
			if p.candidate.Status == "concluded" {
				c.Candidate++
			}
		}
		c.Pass = len(c.Lost) == 0
		out.Criteria = append(out.Criteria, c)
	}
	var pooledCand, pooledBase []float64
	for _, name := range g.Gated {
		cand, base := worstR7(pairs[name])
		if slices.Contains(g.Pooled, name) {
			pooledCand, pooledBase = append(pooledCand, cand...), append(pooledBase, base...)
		}
		for _, q := range g.Quantiles {
			c := criterion{Kind: quantileLabel(q), Scenario: name}
			c.Baseline, c.Candidate, c.Lo, c.Hi = quantileChange(cand, base, q, g.Confidence, g.Resamples, g.BootstrapSeed)
			c.Change = c.Candidate - c.Baseline
			c.Pass = c.Lo <= 0
			out.Criteria = append(out.Criteria, c)
		}
		c := criterion{Kind: "time", Scenario: name}
		var logs float64
		for _, p := range pairs[name] {
			if p.candidate.Status == "concluded" && p.baseline.Status == "concluded" && p.candidate.SimHours > 0 && p.baseline.SimHours > 0 {
				logs += math.Log(p.candidate.SimHours / p.baseline.SimHours)
				c.Timed++
			}
		}
		if c.Timed > 0 {
			c.Change = math.Exp(logs / float64(c.Timed))
			c.Pass = c.Change <= g.MaxTimeRatio
		}
		out.Criteria = append(out.Criteria, c)
	}
	if len(g.Pooled) > 0 {
		slices.Sort(pooledCand)
		slices.Sort(pooledBase)
		c := criterion{Kind: "pooled_median", Scenario: strings.Join(g.Pooled, "+"), Baseline: percentile(pooledBase, 0.5), Candidate: percentile(pooledCand, 0.5)}
		c.Change = c.Candidate - c.Baseline
		c.Pass = c.Candidate < c.Baseline
		out.Criteria = append(out.Criteria, c)
	}
	return out
}

func worstR7(pairs []pair) (candidate, baseline []float64) {
	for _, p := range pairs {
		candidate = append(candidate, *p.candidate.WorstR7HazardPerH)
		baseline = append(baseline, *p.baseline.WorstR7HazardPerH)
	}
	return candidate, baseline
}

func quantileLabel(q float64) string {
	if q == 0.5 {
		return "median"
	}
	return fmt.Sprintf("p%.0f", q*100)
}

// quantileChange returns the q-quantile of each side and the bootstrap interval of the candidate's
// minus the baseline's, resampling pairs: each resample draws len(candidate) pair indices with
// replacement from a fresh PCG seeded with seed, so the interval is the same on every run.
func quantileChange(candidate, baseline []float64, q, confidence float64, resamples int, seed [2]uint64) (base, cand, lo, hi float64) {
	n := len(candidate)
	a, b := slices.Sorted(slices.Values(candidate)), slices.Sorted(slices.Values(baseline))
	base, cand = percentile(b, q), percentile(a, q)
	rng := rand.New(rand.NewPCG(seed[0], seed[1]))
	boot := make([]float64, resamples)
	a, b = make([]float64, n), make([]float64, n)
	for i := range boot {
		for j := range n {
			k := rng.IntN(n)
			a[j], b[j] = candidate[k], baseline[k]
		}
		slices.Sort(a)
		slices.Sort(b)
		boot[i] = percentile(a, q) - percentile(b, q)
	}
	slices.Sort(boot)
	tail := (1 - confidence) / 2
	return base, cand, percentile(boot, tail), percentile(boot, 1-tail)
}

// judgeAndReport prints the gate's verdict and returns errGateFailed unless it passed, so the
// command exits nonzero on a failed or refused gate.
func judgeAndReport(w io.Writer, g *gate, candidate, baseline []result) error {
	verdict := judgeGate(g, candidate, baseline)
	reportGate(w, g, verdict)
	if !verdict.pass() {
		return errGateFailed
	}
	return nil
}

func reportGate(w io.Writer, g *gate, r gateResult) {
	fmt.Fprintf(w, "gate %s: judged against ruleset %d at %s on every %s seed; quantiles of worst_r7_hazard_per_h fail only when the %g%% interval of candidate minus baseline (%d paired resamples, PCG(%d,%d)) lies above 0; time is the geometric mean ratio over pairs both concluded.\n", g.ID, g.Baseline.Ruleset, strings.Join(g.Baseline.Commits, " or "), g.Split, g.Confidence*100, g.Resamples, g.BootstrapSeed[0], g.BootstrapSeed[1])
	if g.Note != "" {
		fmt.Fprintf(w, "gate %s: %s\n", g.ID, g.Note)
	}
	if r.Refused != "" {
		fmt.Fprintf(w, "gate %s: FAIL refused: %s\n", g.ID, r.Refused)
		return
	}
	failed := 0
	for _, c := range r.Criteria {
		result := "PASS"
		if !c.Pass {
			result = "FAIL"
			failed++
		}
		switch c.Kind {
		case "conclusion":
			lost := make([]string, len(c.Lost))
			for i, seed := range c.Lost {
				lost[i] = fmt.Sprint(seed)
			}
			fmt.Fprintf(w, "gate conclusion %s baseline_concluded=%g candidate_concluded=%g lost=%d lost_seeds=[%s] threshold=lost==0 %s\n", c.Scenario, c.Baseline, c.Candidate, len(c.Lost), strings.Join(lost, ","), result)
		case "time":
			fmt.Fprintf(w, "gate time %s ratio=%.4f timed=%d threshold=ratio<=%g %s\n", c.Scenario, c.Change, c.Timed, g.MaxTimeRatio, result)
		case "pooled_median":
			fmt.Fprintf(w, "gate pooled_median %s baseline=%.6f candidate=%.6f change=%+.6f threshold=change<0 %s\n", c.Scenario, c.Baseline, c.Candidate, c.Change, result)
		default:
			fmt.Fprintf(w, "gate %s %s baseline=%.6f candidate=%.6f change=%+.6f ci=[%+.6f,%+.6f] threshold=ci_low<=0 %s\n", c.Kind, c.Scenario, c.Baseline, c.Candidate, c.Change, c.Lo, c.Hi, result)
		}
	}
	if failed > 0 {
		fmt.Fprintf(w, "gate %s: FAIL %d of %d criteria\n", g.ID, failed, len(r.Criteria))
		return
	}
	fmt.Fprintf(w, "gate %s: PASS %d criteria\n", g.ID, len(r.Criteria))
}
