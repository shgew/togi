package machine

import (
	"fmt"
	"iter"
	"math/rand/v2"
	"strings"
	"time"
)

var LoadStepPeriods = [...]time.Duration{10 * time.Millisecond, 50 * time.Millisecond, 200 * time.Millisecond, time.Second, 5 * time.Second}

const DutyPeriod = 100 * time.Millisecond

const loadStepStream uint64 = 0x6c6f616473746570

// LoadSchedule is an R3 schedule when Seed is set and an R4 schedule when DutyPct is set.
type LoadSchedule struct {
	Seed    uint64
	DutyPct int
}

func ScheduleFor(spec TrialSpec) *LoadSchedule {
	switch spec.Regime {
	case R3:
		return &LoadSchedule{Seed: spec.Seed}
	case R4:
		return &LoadSchedule{DutyPct: spec.Workload.DutyPct}
	case R1, R2, R5, R6, R7:
	}
	return nil
}

func (s LoadSchedule) String() string {
	if s.DutyPct > 0 {
		return fmt.Sprintf("%d%% duty, %s period", s.DutyPct, DutyPeriod)
	}
	periods := make([]string, len(LoadStepPeriods))
	for i, p := range LoadStepPeriods {
		periods[i] = p.String()
	}
	return "random on/off periods from " + strings.Join(periods, ", ")
}

// Periods yields on, off, on, off, ... forever. Each on period ends with SIGSTOP and each off period with SIGCONT.
func (s LoadSchedule) Periods() iter.Seq[time.Duration] {
	if s.DutyPct > 0 {
		on := time.Duration(s.DutyPct) * DutyPeriod / 100
		return func(yield func(time.Duration) bool) {
			for yield(on) && yield(DutyPeriod-on) {
			}
		}
	}
	return func(yield func(time.Duration) bool) {
		r := rand.New(rand.NewPCG(s.Seed, loadStepStream))
		for yield(LoadStepPeriods[r.IntN(len(LoadStepPeriods))]) {
		}
	}
}

func (s LoadSchedule) Counts(ran time.Duration) (stops, conts int) {
	var t time.Duration
	on := true
	for p := range s.Periods() {
		t += p
		if t >= ran {
			return stops, conts
		}
		if on {
			stops++
		} else {
			conts++
		}
		on = !on
	}
	return stops, conts
}
