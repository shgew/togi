package main

import (
	"math"

	"github.com/shgew/togi/internal/machine"
)

func (l *likelihood) fitCCD() {
	c := l.cfg.CCD
	indices := l.selectObs(func(o observation) bool { return o.spec.Regime == machine.R7 })
	for range 12 {
		before := l.score(indices)
		l.continuous(func() float64 { return math.Exp(c.LogRate) }, func(x float64) { c.LogRate = math.Log(max(x, 1e-12)) }, indices, 1e-7, 0.1)
		l.continuous(func() float64 { return c.Slope }, func(x float64) { c.Slope = x }, indices, 1e-4, 0.5)
		for ccd := range 2 {
			l.continuous(func() float64 { return math.Exp(c.Effect[ccd]) }, func(x float64) { c.Effect[ccd] = math.Log(max(x, math.Exp(-5))) }, indices, math.Exp(-5), math.Exp(5))
		}
		if before-l.score(indices) < 1e-5 {
			break
		}
	}
}
