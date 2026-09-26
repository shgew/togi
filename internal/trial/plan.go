package trial

import (
	"iter"
	"slices"
	"time"

	"code.marleb.org/shgew/shycler/internal/machine"
	"code.marleb.org/shgew/shycler/internal/tuner"
)

type toggle struct {
	At        time.Duration
	Stop      bool
	Instances []int
}

func plan(spec machine.TrialSpec, cores []machine.CoreInfo) iter.Seq[toggle] {
	return func(yield func(toggle) bool) {
		switch spec.Regime {
		case machine.R3, machine.R4:
			var at time.Duration
			stop := true
			for period := range machine.ScheduleFor(spec).Periods() {
				at += period
				if at >= spec.Duration {
					return
				}
				if !yield(toggle{At: at, Stop: stop, Instances: []int{0}}) {
					return
				}
				stop = !stop
			}
		case machine.R6:
			selected := make([]machine.CoreInfo, 0, len(spec.Cores))
			for _, c := range cores {
				if slices.Contains(spec.Cores, c.Core) {
					selected = append(selected, c)
				}
			}
			order := tuner.Order(selected)
			if len(order) == 0 {
				return
			}
			for k, at := 0, spec.Duration/2; at < spec.Duration; k, at = k+1, at+2*time.Second {
				index := slices.Index(spec.Cores, order[k%len(order)])
				if index < 0 {
					continue
				}
				if !yield(toggle{At: at, Instances: []int{index}}) {
					return
				}
				if at+100*time.Millisecond < spec.Duration {
					if !yield(toggle{At: at + 100*time.Millisecond, Stop: true, Instances: []int{index}}) {
						return
					}
				}
			}
		case machine.R1, machine.R2, machine.R5, machine.R7:
		}
	}
}
