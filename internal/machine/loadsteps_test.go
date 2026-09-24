package machine

import (
	"slices"
	"testing"
	"time"
)

func TestLoadSchedule(t *testing.T) {
	t.Parallel()
	duty := func(pct int) LoadSchedule { return LoadSchedule{DutyPct: pct} }
	counts := []struct {
		s            LoadSchedule
		ran          time.Duration
		stops, conts int
	}{
		{duty(25), time.Second, 10, 9},
		{duty(50), time.Second, 10, 9},
		{duty(25), 25 * time.Millisecond, 0, 0},
		{duty(25), 26 * time.Millisecond, 1, 0},
		{duty(75), 0, 0, 0},
	}
	for _, c := range counts {
		if stops, conts := c.s.Counts(c.ran); stops != c.stops || conts != c.conts {
			t.Errorf("%s over %s: %d stops, %d continues; want %d, %d", c.s, c.ran, stops, conts, c.stops, c.conts)
		}
	}

	first := func(s LoadSchedule, n int) []time.Duration {
		var out []time.Duration
		for p := range s.Periods() {
			if out = append(out, p); len(out) == n {
				break
			}
		}
		return out
	}
	five := LoadSchedule{Seed: 5}
	a, b := first(five, 100), first(five, 100)
	if !slices.Equal(a, b) {
		t.Fatal("seed 5 yields two different schedules")
	}
	if slices.Equal(a, first(LoadSchedule{Seed: 6}, 100)) {
		t.Fatal("seeds 5 and 6 yield the same schedule")
	}
	for _, p := range a {
		if !slices.Contains(LoadStepPeriods[:], p) {
			t.Fatalf("period %s is not a load-step period", p)
		}
	}
	for ran := time.Duration(0); ran <= 90*time.Second; ran += 10 * time.Millisecond {
		if stops, conts := five.Counts(ran); stops != conts && stops != conts+1 {
			t.Fatalf("after %s: %d stops, %d continues", ran, stops, conts)
		}
	}

	if got := (LoadSchedule{DutyPct: 50}).String(); got != "50% duty, 100ms period" {
		t.Errorf("R4 schedule %q", got)
	}
	if got := five.String(); got != "random on/off periods from 10ms, 50ms, 200ms, 1s, 5s" {
		t.Errorf("R3 schedule %q", got)
	}
	for _, r := range []Regime{R1, R2, R5} {
		if s := ScheduleFor(TrialSpec{Regime: r}); s != nil {
			t.Errorf("ScheduleFor(%s) = %+v, want nil", r, s)
		}
	}
	if s := ScheduleFor(TrialSpec{Regime: R3, Seed: 812}); s == nil || *s != (LoadSchedule{Seed: 812}) {
		t.Errorf("ScheduleFor(R3) = %+v", s)
	}
	if s := ScheduleFor(TrialSpec{Regime: R4, Workload: PickWorkload(R4, 1)}); s == nil || *s != (LoadSchedule{DutyPct: 50}) {
		t.Errorf("ScheduleFor(R4) = %+v", s)
	}
}
