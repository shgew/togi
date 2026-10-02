package main

import (
	"fmt"
	"io"
	"math"
	"math/rand/v2"
	"slices"
	"sort"
)

type key struct {
	scenario string
	seed     uint64
}
type pair struct{ candidate, baseline result }
type comparison struct {
	Pairs                                    int
	Timed                                    int
	Ratio, Lo, Hi                            float64
	Faster, Slower, Equal                    int
	CrashDelta, DepthDelta, MaxHazardDelta   float64
	V1, V2, V3, V4                           int
	RealAnswerShare, BaselineRealAnswerShare float64
}

func pairing(candidate, baseline []result) []pair {
	base := make(map[key]result, len(baseline))
	for _, r := range baseline {
		base[key{r.Scenario, r.Seed}] = r
	}
	var pairs []pair
	for _, r := range candidate {
		if b, ok := base[key{r.Scenario, r.Seed}]; ok {
			pairs = append(pairs, pair{r, b})
		}
	}
	sort.Slice(pairs, func(i, j int) bool {
		if pairs[i].candidate.Scenario != pairs[j].candidate.Scenario {
			return pairs[i].candidate.Scenario < pairs[j].candidate.Scenario
		}
		return pairs[i].candidate.Seed < pairs[j].candidate.Seed
	})
	return pairs
}

func compare(pairs []pair) comparison {
	c := comparison{Pairs: len(pairs), Ratio: 1, Lo: 1, Hi: 1}
	logs := make(map[string][]float64)
	depths := make(map[string]struct {
		sum float64
		n   int
	})
	var real, trials, baseReal, baseTrials int
	for i, p := range pairs {
		a, b := p.candidate, p.baseline
		real += a.RealAnswers
		trials += a.Trials
		baseReal += b.RealAnswers
		baseTrials += b.Trials
		if b.Status == "concluded" && a.Status != "concluded" {
			c.V1++
		}
		dh := a.HazardMaxPerH - b.HazardMaxPerH
		if dh > 0.01 {
			c.V2++
		}
		if i == 0 || dh > c.MaxHazardDelta {
			c.MaxHazardDelta = dh
		}
		dd := float64(a.Depth - b.Depth)
		if dd > 5 {
			c.V3++
		}
		d := depths[a.Scenario]
		d.sum += dd
		d.n++
		depths[a.Scenario] = d
		c.CrashDelta += float64(a.Crashes - b.Crashes)
		c.DepthDelta += dd
		if a.Status == "concluded" && b.Status == "concluded" && a.SimHours > 0 && b.SimHours > 0 {
			logs[a.Scenario] = append(logs[a.Scenario], math.Log(a.SimHours/b.SimHours))
			switch {
			case a.SimHours < b.SimHours:
				c.Faster++
			case a.SimHours > b.SimHours:
				c.Slower++
			default:
				c.Equal++
			}
		}
	}
	c.RealAnswerShare = answerShare(real, trials)
	c.BaselineRealAnswerShare = answerShare(baseReal, baseTrials)
	for _, d := range depths {
		if d.sum/float64(d.n) > 1 {
			c.V3++
		}
	}
	if c.Pairs > 0 {
		c.CrashDelta /= float64(c.Pairs)
		c.DepthDelta /= float64(c.Pairs)
	}
	names := make([]string, 0, len(logs))
	for name := range logs {
		names = append(names, name)
	}
	slices.Sort(names)
	var sum float64
	for _, name := range names {
		values := logs[name]
		var scenarioSum float64
		for _, value := range values {
			scenarioSum += value
		}
		mean := scenarioSum / float64(len(values))
		sum += mean
		c.Timed += len(values)
		if name == "target" && math.Exp(mean) > 1 {
			c.V4++
		}
	}
	if c.Timed == 0 {
		return c
	}
	c.Ratio = math.Exp(sum / float64(len(names)))
	rng := rand.New(rand.NewPCG(1, 1))
	boot := make([]float64, 10000)
	for i := range boot {
		var s float64
		for _, name := range names {
			values := logs[name]
			var scenarioSum float64
			for range values {
				scenarioSum += values[rng.IntN(len(values))]
			}
			s += scenarioSum / float64(len(values))
		}
		boot[i] = math.Exp(s / float64(len(names)))
	}
	slices.Sort(boot)
	c.Lo, c.Hi = percentile(boot, 0.025), percentile(boot, 0.975)
	return c
}

func percentile(sorted []float64, p float64) float64 {
	at := p * float64(len(sorted)-1)
	lo := int(at)
	hi := min(lo+1, len(sorted)-1)
	return sorted[lo] + (sorted[hi]-sorted[lo])*(at-float64(lo))
}

func verdict(c comparison) string {
	if c.V1+c.V2+c.V3+c.V4 > 0 || c.Lo > 1 {
		return "REJECT"
	}
	if c.Timed > 0 && c.Hi < 1 {
		return "ACCEPT"
	}
	return "NEUTRAL"
}

func printComparison(w io.Writer, label string, c comparison) {
	fmt.Fprintf(w, "%s ratio=%.3f ci=[%.3f,%.3f] pairs=%d timed=%d faster=%d slower=%d equal=%d crash_delta=%.3f depth_delta=%.3f max_hazard_delta=%.6f real_answer_share=%.6f baseline_real_answer_share=%.6f violations=V1:%d,V2:%d,V3:%d,V4:%d\n", label, c.Ratio, c.Lo, c.Hi, c.Pairs, c.Timed, c.Faster, c.Slower, c.Equal, c.CrashDelta, c.DepthDelta, c.MaxHazardDelta, c.RealAnswerShare, c.BaselineRealAnswerShare, c.V1, c.V2, c.V3, c.V4)
}

func reportComparison(w io.Writer, candidate, baseline []result) {
	fmt.Fprintln(w, "comparison: overall ratio weights scenarios equally; CI resamples pairs within each scenario (10000, fixed seed). V1=lost conclusion; V2=hazard increase >0.01/h; V3=mean depth increase >1 or pair >5; V4=target ratio >1. Positive depth delta is shallower.")
	pairs := pairing(candidate, baseline)
	splits := make(map[string]bool)
	for _, r := range candidate {
		splits[r.Split] = true
	}
	comparable := 0
	for _, r := range baseline {
		if splits[r.Split] {
			comparable++
		}
	}
	if len(pairs) != len(candidate) || len(pairs) != comparable {
		fmt.Fprintf(w, "comparison: WARNING %d candidate and %d baseline runs in the same splits unmatched; %d paired\n", len(candidate)-len(pairs), comparable-len(pairs), len(pairs))
	}
	groups := make(map[string][]pair)
	for _, p := range pairs {
		groups[p.candidate.Scenario] = append(groups[p.candidate.Scenario], p)
	}
	names := make([]string, 0, len(groups))
	for name := range groups {
		names = append(names, name)
	}
	slices.Sort(names)
	for _, name := range names {
		printComparison(w, name, compare(groups[name]))
	}
	c := compare(pairs)
	printComparison(w, "verdict: "+verdict(c), c)
}

func reportSummary(w io.Writer, results []result) {
	fmt.Fprintln(w, "summary: time, crashes and depth include all run statuses; hazard is steady-state failures/hour with all cores loaded.")
	fmt.Fprintln(w, "scenario          runs concluded median_h mean_h median_crashes mean_depth max_hazard/h real_answer_share")
	groups := make(map[string][]result)
	var names []string
	for _, r := range results {
		if _, ok := groups[r.Scenario]; !ok {
			names = append(names, r.Scenario)
		}
		groups[r.Scenario] = append(groups[r.Scenario], r)
	}
	for _, name := range names {
		summaryRow(w, name, groups[name])
	}
	summaryRow(w, "TOTAL", results)
}

func summaryRow(w io.Writer, name string, rows []result) {
	if len(rows) == 0 {
		return
	}
	hours, crashes := make([]float64, 0, len(rows)), make([]float64, 0, len(rows))
	var concluded, real, trials int
	var sum, depth, hazard float64
	for _, r := range rows {
		real += r.RealAnswers
		trials += r.Trials
		if r.Status == "concluded" {
			concluded++
		}
		hours = append(hours, r.SimHours)
		crashes = append(crashes, float64(r.Crashes))
		sum += r.SimHours
		depth += float64(r.Depth)
		hazard = max(hazard, r.HazardMaxPerH)
	}
	slices.Sort(hours)
	slices.Sort(crashes)
	fmt.Fprintf(w, "%-17s %4d %9d %8.3f %8.3f %14.1f %10.2f %12.6f %17.6f\n", name, len(rows), concluded, percentile(hours, 0.5), sum/float64(len(rows)), percentile(crashes, 0.5), depth/float64(len(rows)), hazard, answerShare(real, trials))
}
