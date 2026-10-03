package main

import (
	"math"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/tools/trialfacts"
)

func (l *likelihood) fitCCD(cfg *sim.Config) {
	c := cfg.CCD
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

func ccdPrior(records []trialfacts.Record) (logRate, meanDepth float64, supported bool) {
	n, failures := 0, 0
	var exposure, depthExposure float64
	for _, r := range records {
		if r.Class.Regime != machine.R7 {
			continue
		}
		n++
		if r.Outcome == journal.OutcomeFailure {
			failures++
		}
		size := len(r.Profile) / 2
		for ccd := range 2 {
			loaded := false
			for _, core := range r.Class.Cores {
				loaded = loaded || core/size == ccd
			}
			if !loaded {
				continue
			}
			depth := 0
			for core := ccd * size; core < (ccd+1)*size; core++ {
				depth -= r.Profile[core]
			}
			exposure += float64(r.Class.DurationS)
			depthExposure += float64(r.Class.DurationS) * float64(depth) / float64(size)
		}
	}
	if n == 0 || exposure == 0 {
		return 0, 0, false
	}
	p := (float64(failures) + 0.5) / (float64(n) + 1)
	rate := -math.Log1p(-p) * float64(n) / exposure
	return math.Log(rate), depthExposure / exposure, true
}
