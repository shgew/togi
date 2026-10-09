package main

import (
	"math"
	"slices"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/requests"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/tuner"
)

// worstR7HazardPerH is tools/bench's metric behind worst_r7_hazard_per_h, copied verbatim.
func worstR7HazardPerH(m *sim.Machine, profile []int) *float64 {
	var all [16]int
	for core := range all {
		all[core] = core
	}
	spec := machine.TrialSpec{Regime: machine.R7}
	var worst float64
	for _, workload := range machine.Workloads(machine.R7) {
		spec.Workload = workload
		for ccd := range 2 {
			var loaded [8]int
			for i := range loaded {
				loaded[i] = ccd*8 + i
			}
			spec.Cores = loaded[:]
			for len(spec.Cores) >= 2 {
				lanes, ok := m.R7Requests(profile, spec)
				if !ok {
					return nil
				}
				worst = max(worst, m.Hazard(profile, spec)*3600)
				spec.Cores = nextR7Partial(spec.Cores, lanes)
			}
		}
		spec.Cores = all[:]
		worst = max(worst, m.Hazard(profile, spec)*3600)
	}
	return &worst
}

func nextR7Partial(cores []int, lanes [16]float32) []int {
	top := math.Inf(-1)
	for _, core := range cores {
		top = max(top, float64(lanes[core]))
	}
	n := 0
	for _, core := range cores {
		if float64(lanes[core]) < top-requests.TieV {
			cores[n] = core
			n++
		}
	}
	return cores[:n]
}

type confirmed struct {
	hours   float64
	profile []int
}

type replayed struct {
	final     []int
	simHours  float64
	crashes   int
	confirmed []confirmed
}

// replay derives the bench's final profile, sim hours and crashes from a kept journal,
// plus the checked profile at every passed checking cycle end.
func replay(events []journal.Event, cores int) replayed {
	var r replayed
	var start time.Time
	var st journal.State
	t := tuner.New()
	profile := func() []int {
		p := t.Profile()
		if len(p) == 0 {
			p = make([]int, cores)
			if st.Session != nil {
				copy(p, st.Session.Baseline)
			}
		}
		return p
	}
	for _, e := range events {
		st.Fold(e)
		t.Fold(e)
		if e.Kind == journal.KindSessionStart && start.IsZero() {
			start = e.Time
		}
		if start.IsZero() {
			continue
		}
		h := e.Time.Sub(start).Hours()
		r.simHours = h
		switch p := e.Data.(type) {
		case *journal.CheckingCycle:
			if p.Event == journal.CycleEnd && p.Passed {
				r.confirmed = append(r.confirmed, confirmed{h, profile()})
			}
		case *journal.CrashDetected:
			r.crashes++
		}
	}
	t.Project(&st)
	r.final = profile()
	return r
}

func lastConfirmedBy(c []confirmed, hours float64) ([]int, bool) {
	i, _ := slices.BinarySearchFunc(c, hours, func(x confirmed, h float64) int {
		if x.hours <= h {
			return -1
		}
		return 1
	})
	if i == 0 {
		return nil, false
	}
	return c[i-1].profile, true
}
