// Package machine holds the vocabulary shared by every package that tunes or simulates a CPU, and the seams the run loop consumes.
package machine

import "slices"

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

var ConfirmationRegimes = []Regime{R1, R2, R3, R4, R5}

func (r Regime) Valid() bool {
	return slices.Contains(Regimes, r)
}

func (r Regime) AllCores() bool {
	return r == R6 || r == R7
}
