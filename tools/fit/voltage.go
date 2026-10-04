package main

import (
	"maps"
	"math"
	"slices"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/tools/modelcheck"
	"github.com/shgew/togi/tools/trialfacts"
)

const requestReferenceMHz = 5240.0

func median(values []float64) float64 {
	slices.Sort(values)
	n := len(values)
	if n%2 != 0 {
		return values[n/2]
	}
	return (values[n/2-1] + values[n/2]) / 2
}

type requestPoint struct{ offset, clock, voltage float64 }

func voltageConfig(records []trialfacts.Record) *sim.SharedVoltage {
	v := &sim.SharedVoltage{IdleV: .8, MarginV: .006, Rate: .001, PowerLimitW: 192, ThermalLimitW: 192, Workload: make(map[string]sim.VoltageWorkload)}
	for index, workload := range machine.Workloads(machine.R7) {
		w := sim.VoltageWorkload{ReferenceMHz: requestReferenceMHz, FullMHz: [2]float64{5240, 5240}, IdleGainMHz: 30, WattsPerCore: 13, OffsetWattsPerCount: .12, PackageMHzPerW: 13, BalanceMHzPerW: 19.3, Core: make([]sim.VoltageCore, 16)}
		if index != 0 {
			w.FullMHz = [2]float64{5070, 5070}
			w.WattsPerCore, w.PackageMHzPerW, w.BalanceMHzPerW = 15, 5.65, 16.67
		}
		for core := range w.Core {
			w.Core[core] = sim.VoltageCore{BaseV: 1.25, ThresholdV: 1.15, CountV: .0036, ClockVPer100MHz: .033, ThresholdClockVPer100MHz: .033}
		}
		v.Workload[workload.ID] = w
	}
	avx2 := machine.PickWorkload(machine.R7, 0).ID
	fitRequests(v, records, avx2)
	for _, workload := range machine.Workloads(machine.R7)[1:] {
		w := v.Workload[workload.ID]
		prior := v.Workload[avx2]
		copy(w.Core, prior.Core)
		v.Workload[workload.ID] = w
		fitRequests(v, records, workload.ID)
	}
	for _, workload := range machine.Workloads(machine.R7) {
		fitClocks(v, records, workload.ID)
	}
	return v
}

func fitRequests(v *sim.SharedVoltage, records []trialfacts.Record, id string) {
	w := v.Workload[id]
	var points [16][]requestPoint
	for _, r := range records {
		if r.Class.Regime != machine.R7 || r.Class.Workload != id {
			continue
		}
		for core := range 16 {
			voltage, measured := r.VoltageRequestsV[core]
			mhz, ok := r.CCDMHz[core/8]
			if !measured || !ok || !slices.Contains(r.Class.Cores, core) {
				continue
			}
			points[core] = append(points[core], requestPoint{float64(r.Profile[core]), (float64(mhz) - w.ReferenceMHz) / 100, voltage})
		}
	}
	// Within-core centering removes the intercept. Ridge terms express voltage
	// residual SD 2mV and prior SDs .5mV/count and 8mV/100MHz.
	a, b, d := 16.0, 0.0, .0625
	e, f := a*.0036, d*.033
	count := 0
	for _, ps := range points {
		if len(ps) == 0 {
			continue
		}
		count += len(ps)
		var x, y, z float64
		for _, p := range ps {
			x += p.offset
			y += p.clock
			z += p.voltage
		}
		n := float64(len(ps))
		x, y, z = x/n, y/n, z/n
		for _, p := range ps {
			xx, yy, zz := p.offset-x, p.clock-y, p.voltage-z
			a += xx * xx
			b += xx * yy
			d += yy * yy
			e += xx * zz
			f += yy * zz
		}
	}
	if count == 0 {
		return
	}
	countV := min(.005, max(.0025, (e*d-f*b)/(a*d-b*b)))
	clockV := min(.06, max(.005, (a*f-b*e)/(a*d-b*b)))
	var bases [16]float64
	var supported [16]bool
	for core, ps := range points {
		if len(ps) == 0 {
			continue
		}
		residuals := make([]float64, len(ps))
		for i, p := range ps {
			residuals[i] = p.voltage - countV*p.offset - clockV*p.clock
		}
		bases[core], supported[core] = median(residuals), true
	}
	for ccd := range 2 {
		var values []float64
		for core := ccd * 8; core < (ccd+1)*8; core++ {
			if supported[core] {
				values = append(values, bases[core])
			}
		}
		if len(values) == 0 {
			continue
		}
		mean := median(values)
		for core := ccd * 8; core < (ccd+1)*8; core++ {
			n := float64(len(points[core]))
			w.Core[core].BaseV = min(1.5, max(.9, (n*bases[core]+5*mean)/(n+5)))
			w.Core[core].CountV = countV
			w.Core[core].ClockVPer100MHz = clockV
			w.Core[core].ThresholdClockVPer100MHz = clockV
			w.Core[core].ThresholdV = w.Core[core].BaseV - .1
		}
	}
	v.Workload[id] = w
}

func fitClocks(v *sim.SharedVoltage, records []trialfacts.Record, id string) {
	w := v.Workload[id]
	var full [2][]float64
	var partial [2][]float64
	for _, r := range records {
		if r.Class.Regime != machine.R7 || r.Class.Workload != id {
			continue
		}
		var counts [2]int
		var zero [2]bool
		for _, core := range r.Class.Cores {
			counts[core/8]++
			zero[core/8] = zero[core/8] || r.Profile[core] == 0
		}
		// Parked zero-offset cores are power-limited and must not lower the
		// unconstrained full-CCD intercept. All-core pressure is fitted below.
		for ccd, clock := range r.CCDMHz {
			if zero[ccd] || counts[1-ccd] > 0 {
				continue
			}
			switch counts[ccd] {
			case 8:
				full[ccd] = append(full[ccd], float64(clock))
			case 7:
				partial[ccd] = append(partial[ccd], float64(clock))
			}
		}
	}
	var gains []float64
	for ccd := range 2 {
		if len(full[ccd]) > 0 {
			w.FullMHz[ccd] = min(6000, max(4000, median(full[ccd])))
		}
		if len(full[ccd]) > 0 && len(partial[ccd]) > 0 {
			gains = append(gains, median(partial[ccd])-w.FullMHz[ccd])
		}
	}
	if len(gains) > 0 {
		w.IdleGainMHz = min(60, max(0, median(gains)))
	}
	// Fix the watt scale; clocks identify MHz/W responses, not watts and
	// coefficients separately. Fit package/balance responses only when seen.
	var packageResponses, balanceResponses []float64
	for _, r := range records {
		if r.Class.Regime != machine.R7 || r.Class.Workload != id || len(r.Class.Cores) != 16 || len(r.CCDMHz) != 2 {
			continue
		}
		var watts [2]float64
		for _, core := range r.Class.Cores {
			watts[core/8] += max(0, w.WattsPerCore+w.OffsetWattsPerCount*float64(r.Profile[core]+35))
		}
		pressure := max(0, watts[0]+watts[1]-min(v.PowerLimitW, v.ThermalLimitW))
		if pressure > 1 {
			packageResponses = append(packageResponses, (w.FullMHz[0]+w.FullMHz[1]-float64(r.CCDMHz[0]+r.CCDMHz[1]))/(2*pressure))
		}
		if math.Abs(watts[0]-watts[1]) > 1 {
			balanceResponses = append(balanceResponses, (w.FullMHz[0]-w.FullMHz[1]-float64(r.CCDMHz[0]-r.CCDMHz[1]))/(watts[0]-watts[1]))
		}
	}
	if len(packageResponses) > 0 {
		w.PackageMHzPerW = min(30, max(0, median(packageResponses)))
	}
	if len(balanceResponses) > 0 {
		w.BalanceMHzPerW = min(30, max(0, median(balanceResponses)))
	}
	v.Workload[id] = w
}

func voltagePenalty(v *sim.SharedVoltage) float64 {
	var penalty float64
	for _, workload := range machine.Workloads(machine.R7)[:2] {
		w := v.Workload[workload.ID]
		for ccd := range 2 {
			var mean float64
			for core := ccd * 8; core < (ccd+1)*8; core++ {
				mean += w.Core[core].ThresholdV / 8
			}
			for core := ccd * 8; core < (ccd+1)*8; core++ {
				deviation := (w.Core[core].ThresholdV - mean) / .01
				penalty += .5 * deviation * deviation
			}
		}
	}
	width := math.Log(v.MarginV/.006) / math.Ln2
	return penalty + .5*width*width
}

func (l *voltageLikelihood) fitVoltage(cfg *sim.Config) {
	v := cfg.SharedVoltage
	indices := l.selectObs(func(o observation) bool { return o.spec.Regime == machine.R7 && len(o.spec.Cores) > 1 })
	if len(indices) == 0 {
		return
	}
	choose := func(get func() float64, set func(float64), low, high float64) {
		best, score := get(), l.score(indices)
		evaluate := func(x float64) float64 {
			set(x)
			syncYcruncher(v)
			l.rebuild()
			candidate := l.score(indices)
			if candidate < score-1e-9 {
				best, score = x, candidate
			}
			return candidate
		}
		const ratio = .6180339887498949
		a, b := low, high
		x, y := b-ratio*(b-a), a+ratio*(b-a)
		fx, fy := evaluate(x), evaluate(y)
		for range 20 {
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
		syncYcruncher(v)
		l.rebuild()
	}
	for _, workload := range machine.Workloads(machine.R7)[:2] {
		w := v.Workload[workload.ID]
		for ccd := range 2 {
			var original [8]float64
			low, high := -.10, .10
			for i := range 8 {
				original[i] = w.Core[ccd*8+i].ThresholdV
				low = max(low, .8-original[i])
				high = min(high, 1.5-original[i])
			}
			var delta float64
			choose(func() float64 { return delta }, func(x float64) {
				delta = x
				for i := range 8 {
					w.Core[ccd*8+i].ThresholdV = original[i] + x
				}
			}, low, high)
		}
		for core := range w.Core {
			dst := &w.Core[core].ThresholdV
			choose(func() float64 { return *dst }, func(x float64) { *dst = x }, max(.8, *dst-.03), min(1.5, *dst+.03))
		}
	}
	choose(func() float64 { return math.Log(v.Rate) }, func(x float64) { v.Rate = math.Exp(x) }, math.Log(1e-5), math.Log(.03))
	choose(func() float64 { return math.Log(v.MarginV) }, func(x float64) { v.MarginV = math.Exp(x) }, math.Log(.001), math.Log(.03))
	choose(func() float64 { return v.BackgroundRate }, func(x float64) { v.BackgroundRate = x }, 0, .001)
}

func syncYcruncher(v *sim.SharedVoltage) {
	a := v.Workload[machine.PickWorkload(machine.R7, 0).ID]
	b := v.Workload[machine.PickWorkload(machine.R7, 1).ID]
	id := machine.PickWorkload(machine.R7, 2).ID
	y := v.Workload[id]
	for core := range y.Core {
		y.Core[core].ThresholdV = max(a.Core[core].ThresholdV, b.Core[core].ThresholdV)
	}
	v.Workload[id] = y
}

type namedVoltageFailure struct {
	profile []int
	spec    machine.TrialSpec
	core    int
}

type voltageLikelihood struct {
	likelihood
	named []namedVoltageFailure
}

func namedVoltageFailures(records []trialfacts.Record) []namedVoltageFailure {
	var named []namedVoltageFailure
	for _, r := range records {
		if r.Class.Regime != machine.R7 || len(r.Class.Cores) < 2 || r.Outcome != journal.OutcomeFailure {
			continue
		}
		core := r.Core
		if core == nil {
			core = r.StalledCore
		}
		if core == nil || !slices.Contains(r.Class.Cores, *core) {
			continue
		}
		named = append(named, namedVoltageFailure{
			profile: r.Profile,
			spec: machine.TrialSpec{
				Regime: machine.R7, Workload: machine.Workload{ID: r.Class.Workload},
				Cores: r.Class.Cores, Duration: time.Duration(r.Class.DurationS) * time.Second,
			},
			core: *core,
		})
	}
	return named
}

func (l *voltageLikelihood) score(indices []int) float64 {
	if !l.admissible() {
		return math.Inf(1)
	}
	loss := l.rawScore(indices) + voltagePenalty(l.cfg.SharedVoltage)
	// Conditional on a failure, competing stationary hazards identify its core.
	// All anchor hazards have the same onset exposure; signal weights are fixed.
	for _, failure := range l.named {
		share := l.m.R7FailureShare(failure.profile, failure.spec, failure.core)
		if share <= 0 {
			return math.Inf(1)
		}
		loss -= math.Log(share)
	}
	return loss
}

// fitSharedVoltage is separate from fit: in-sample anchoring must not silently
// change either the legacy target-fit ensemble or the forward-chained model.
func fitSharedVoltage(records []trialfacts.Record) (sim.Config, float64) {
	var legacy []trialfacts.Record
	for _, r := range records {
		if r.Class.Regime != machine.R7 || len(r.Class.Cores) < 2 {
			legacy = append(legacy, r)
		}
	}
	cfg := initialConfig(records)
	if len(legacy) > 0 {
		cfg, _ = fit(legacy)
	}
	cfg.Joints, cfg.CCD = nil, nil
	cfg.SharedVoltage = voltageConfig(records)
	syncYcruncher(cfg.SharedVoltage)
	var l voltageLikelihood
	l.cfg, l.obs, l.named = cfg, aggregate(records), namedVoltageFailures(records)
	l.rebuild()
	all := l.selectObs(func(observation) bool { return true })
	best, previous := cloneSharedFit(cfg), math.Inf(1)
	for range 20 {
		l.fitVoltage(&cfg)
		l.fitHazardShape(cfg.Model, all)
		l.fitFlat(&cfg)
		l.fitIdle(&cfg)
		l.fitLimitRateShift(&cfg, all)
		score := l.score(all)
		if previous-score < 1e-5 {
			break
		}
		best, previous = cloneSharedFit(cfg), score
	}
	l.cfg = best
	l.rebuild()
	l.constrainRate(records, all)
	return l.cfg, l.rawScore(all)
}

func cloneSharedFit(cfg sim.Config) sim.Config {
	cfg = cloneMachine(cfg)
	v := *cfg.SharedVoltage
	v.Workload = maps.Clone(v.Workload)
	for _, workload := range machine.Workloads(machine.R7) {
		w := v.Workload[workload.ID]
		w.Core = slices.Clone(w.Core)
		for core := range w.Core {
			w.Core[core].Signals = maps.Clone(w.Core[core].Signals)
		}
		v.Workload[workload.ID] = w
	}
	cfg.SharedVoltage = &v
	return cfg
}

// constrainRate preserves voltage geometry while profiling the existing
// electrical rate, then background rate. Original intervals remain unchanged.
func (l *voltageLikelihood) constrainRate(records []trialfacts.Record, all []int) bool {
	if l.constrainTimeRate(records, all, &l.cfg.SharedVoltage.Rate, 1e-5, .03) {
		return true
	}
	return l.constrainTimeRate(records, all, &l.cfg.SharedVoltage.BackgroundRate, 0, .001)
}

func (l *voltageLikelihood) constrainTimeRate(records []trialfacts.Record, all []int, dst *float64, minimumRate, maximumRate float64) bool {
	checker, err := modelcheck.NewChecker(l.cfg, records)
	if err != nil {
		panic(err) // The generator validates records and the fixed reset model.
	}
	if checker.Accepts(l.m) {
		return true
	}
	original := *dst
	set := func(rate float64) {
		*dst = rate
		l.rebuild()
	}
	low, high := minimumRate, maximumRate
	found := false
	for range 64 {
		mid := (low + high) / 2
		set(mid)
		check := checker.Check("", "", l.m)
		var tooLow, tooHigh bool
		for _, group := range check.Groups {
			tooLow = tooLow || group.K > group.Interval[1]
			tooHigh = tooHigh || group.K < group.Interval[0]
		}
		if tooLow && tooHigh {
			break // Monotone group probabilities imply disjoint feasible bounds.
		}
		if !tooLow && !tooHigh {
			found = true
			break
		}
		if mid == low || mid == high {
			break
		}
		if tooLow {
			low = mid
		} else {
			high = mid
		}
	}
	if !found {
		set(original)
		return false
	}
	seed := *dst
	low, high = minimumRate, seed
	set(low)
	if !checker.Accepts(l.m) {
		for range 52 {
			mid := (low + high) / 2
			set(mid)
			if checker.Accepts(l.m) {
				high = mid
			} else {
				low = mid
			}
		}
		low = high
	}
	minimum := low
	low, high = seed, maximumRate
	set(high)
	if !checker.Accepts(l.m) {
		for range 52 {
			mid := (low + high) / 2
			set(mid)
			if checker.Accepts(l.m) {
				low = mid
			} else {
				high = mid
			}
		}
		high = low
	}
	maximum := high
	set(seed)
	l.guard = checker
	best, score := seed, l.score(all)
	evaluate := func(rate float64) float64 {
		set(rate)
		candidate := l.score(all)
		if candidate < score-1e-9 {
			best, score = rate, candidate
		}
		return candidate
	}
	evaluate(minimum)
	evaluate(maximum)
	a, b := minimum, maximum
	const ratio = .6180339887498949
	x, y := b-ratio*(b-a), a+ratio*(b-a)
	fx, fy := evaluate(x), evaluate(y)
	for range 24 {
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
	return checker.Accepts(l.m)
}
