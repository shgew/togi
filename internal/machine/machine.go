// Package machine holds the vocabulary shared by every package that tunes or simulates a CPU, and the seams the run loop consumes.
package machine

import (
	"maps"
	"slices"
)

const (
	MinOffset = -50
	MaxOffset = 0
)

func ClampOffset(o int) int {
	return min(max(o, MinOffset), MaxOffset)
}

type Regime string

const (
	R1 Regime = "R1"
	R2 Regime = "R2"
	R3 Regime = "R3"
	R4 Regime = "R4"
	R5 Regime = "R5"
	R6 Regime = "R6"
	R7 Regime = "R7"
)

var Regimes = []Regime{R1, R2, R3, R4, R5, R6, R7}

func (r Regime) Valid() bool {
	return slices.Contains(Regimes, r)
}

func (r Regime) InstancePerCore() bool {
	return r == R6 || r == R7
}

func Order(cores []CoreInfo) []int {
	byCCD := map[int][]int{}
	for _, c := range cores {
		byCCD[c.CCD] = append(byCCD[c.CCD], c.Core)
	}
	ccds := slices.Sorted(maps.Keys(byCCD))
	for _, id := range ccds {
		slices.Sort(byCCD[id])
	}
	order := make([]int, 0, len(cores))
	for i := 0; len(order) < len(cores); i++ {
		for _, ccd := range ccds {
			if i < len(byCCD[ccd]) {
				order = append(order, byCCD[ccd][i])
			}
		}
	}
	return order
}
