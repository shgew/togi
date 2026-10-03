package main

import (
	"maps"
	"math"
	"slices"

	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
)

func singleCoreObservation(o observation) bool {
	return len(o.spec.Cores) == 1 && (o.spec.Regime == machine.R1 || o.spec.Regime == machine.R2)
}

// Signed coordinate search is used for log-rate effects, not positive rates.
func (l *likelihood) signed(get func() float64, set func(float64), indices []int, low, high float64) {
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
	const ratio = 0.6180339887498949
	a, b := low, high
	x, y := b-ratio*(b-a), a+ratio*(b-a)
	fx, fy := evaluate(x), evaluate(y)
	for range 28 {
		if fx < fy {
			b, y, fy = y, x, fx
			x = b - ratio*(b-a)
			fx = evaluate(x)
		} else {
			a, x, fx = x, y, fy
			y = a + ratio*(b-a)
			fy = evaluate(y)
		}
	}
	set(best)
}

func (l *likelihood) fitSingleCore(cfg *sim.Config) {
	s := cfg.SingleCore
	if s == nil {
		return
	}
	all := l.selectObs(singleCoreObservation)
	for range 8 {
		previous := l.score(all)
		l.signed(func() float64 { return s.LogRate }, func(x float64) { s.LogRate = x }, all, -20, -2)
		l.signed(func() float64 { return s.Slope }, func(x float64) { s.Slope = x }, all, 0, 1)
		l.continuous(func() float64 { return s.MaxRate }, func(x float64) { s.MaxRate = max(x, 1e-6) }, all, 1e-6, 0.1)
		for core := range s.Core {
			indices := l.selectObs(func(o observation) bool { return singleCoreObservation(o) && o.spec.Cores[0] == core })
			l.signed(func() float64 { return s.Core[core] }, func(x float64) { s.Core[core] = x }, indices, -5, 5)
		}
		for _, workload := range slices.Sorted(maps.Keys(s.Workload)) {
			indices := l.selectObs(func(o observation) bool { return singleCoreObservation(o) && o.spec.Workload.ID == workload })
			l.signed(func() float64 { return s.Workload[workload] }, func(x float64) { s.Workload[workload] = x }, indices, -5, 5)
		}
		if math.Abs(previous-l.score(all)) < 1e-5 {
			break
		}
	}
}
