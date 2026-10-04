// Package requests summarizes the core voltage requests and clocks a trial
// recorded and orders loaded cores by request. Both CCDs share one core
// voltage, and the highest request among the loaded cores sets it.
package requests

import (
	"cmp"
	"iter"
	"maps"
	"math"
	"slices"

	"github.com/shgew/togi/internal/machine"
)

const (
	// WarmupMS skips the samples taken while the load ramps up.
	WarmupMS   = 5000
	MinSamples = 20
	// TieV is the spread within which cores count as one top.
	TieV          = 0.001
	VoltsPerCount = 0.0036
)

type Telemetry struct {
	// Requests holds each loaded core's median request in volts.
	Requests map[int]float64
	// TopRequesters holds each CCD's highest-requesting cores, ties included.
	TopRequesters []int
	// CCDMHz holds each loaded CCD's median over samples of its loaded cores' median clock.
	CCDMHz map[int]int
}

// Summarize uses the samples after the warmup that decode request lanes, and
// returns false when fewer than MinSamples remain.
func Summarize(samples iter.Seq[machine.TrialConditions], cores []int, ccdOf func(core int) int) (Telemetry, bool) {
	if len(cores) == 0 {
		return Telemetry{}, false
	}
	requests := make(map[int][]float64, len(cores))
	clocks := map[int][]float64{}
	count := 0
	for sample := range samples {
		if sample.ElapsedMS < WarmupMS || sample.PMTable == nil {
			continue
		}
		lanes := &sample.PMTable.VoltageRequestV
		if slices.ContainsFunc(cores, func(core int) bool { return core < 0 || core >= len(lanes) }) {
			return Telemetry{}, false
		}
		perCCD := map[int][]float64{}
		for _, core := range cores {
			requests[core] = append(requests[core], float64(lanes[core]))
			if mhz, ok := sample.CoreMHz[core]; ok {
				perCCD[ccdOf(core)] = append(perCCD[ccdOf(core)], float64(mhz))
			}
		}
		for ccd, mhz := range perCCD {
			clocks[ccd] = append(clocks[ccd], median(mhz))
		}
		count++
	}
	if count < MinSamples {
		return Telemetry{}, false
	}
	t := Telemetry{Requests: make(map[int]float64, len(cores))}
	byCCD := map[int]map[int]float64{}
	for core, values := range requests {
		t.Requests[core] = median(values)
		ccd := ccdOf(core)
		if byCCD[ccd] == nil {
			byCCD[ccd] = map[int]float64{}
		}
		byCCD[ccd][core] = t.Requests[core]
	}
	for _, group := range byCCD {
		t.TopRequesters = append(t.TopRequesters, Groups(group)[0]...)
	}
	slices.Sort(t.TopRequesters)
	for ccd, mhz := range clocks {
		if len(mhz) < MinSamples {
			continue
		}
		if t.CCDMHz == nil {
			t.CCDMHz = map[int]int{}
		}
		t.CCDMHz[ccd] = int(math.Round(median(mhz)))
	}
	return t, true
}

// Groups orders cores by request, highest first. Each group holds the highest
// remaining core and every remaining core within TieV of it, sorted by core.
func Groups(requests map[int]float64) [][]int {
	cores := slices.SortedFunc(maps.Keys(requests), func(a, b int) int {
		if c := cmp.Compare(requests[b], requests[a]); c != 0 {
			return c
		}
		return cmp.Compare(a, b)
	})
	var groups [][]int
	for len(cores) > 0 {
		n := 1
		for n < len(cores) && requests[cores[n]] >= requests[cores[0]]-TieV {
			n++
		}
		group := slices.Clone(cores[:n])
		slices.Sort(group)
		groups = append(groups, group)
		cores = cores[n:]
	}
	return groups
}

// Shift moves requests measured on one profile to another: a shallower offset
// raises a core's request by VoltsPerCount per count.
func Shift(requests map[int]float64, measured, current []int) map[int]float64 {
	shifted := make(map[int]float64, len(requests))
	for core, v := range requests {
		shifted[core] = v + VoltsPerCount*float64(current[core]-measured[core])
	}
	return shifted
}

// Counts returns the counts shallower that raise a request from v to at least
// target, and at least one.
func Counts(v, target float64) int {
	return max(1, int(math.Ceil((target-v)/VoltsPerCount-1e-9)))
}

func Top(requests map[int]float64) (float64, bool) {
	if len(requests) == 0 {
		return 0, false
	}
	return slices.Max(slices.Collect(maps.Values(requests))), true
}

func median(values []float64) float64 {
	sorted := slices.Sorted(slices.Values(values))
	middle := len(sorted) / 2
	if len(sorted)%2 == 0 {
		return (sorted[middle-1] + sorted[middle]) / 2
	}
	return sorted[middle]
}
