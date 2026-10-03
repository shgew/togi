package sim

import (
	"context"
	"math"
	"testing"
	"time"

	"github.com/shgew/togi/internal/machine"
)

func TestSingleCoreProbabilityAndDraws(t *testing.T) {
	model := DefaultModel()
	model.Signals = map[machine.Signal]float64{machine.ComputationError: 1}
	cfg := Config{Cores: 2, Edges: flat(2, -50, -50), Model: &model, SingleCore: &SingleCore{LogRate: math.Log(0.002), Slope: 0.2, Core: []float64{0, 0.5}, Workload: map[string]float64{"heavy": 1}}}
	m := newMachine(t, cfg)
	spec := machine.TrialSpec{Regime: machine.R2, Cores: []int{0}, Workload: machine.Workload{ID: "heavy"}, Duration: 90*time.Second}
	profile := []int{-25, 0}
	wantRate := 0.002*math.E
	want := -math.Expm1(-90*wantRate)
	if got := m.FailureProbability(profile, spec); math.Abs(got-want) > 1e-12 {
		t.Fatalf("p=%g want %g", got, want)
	}
	if got := m.Hazard(profile, spec); math.Abs(got-wantRate) > 1e-12 {
		t.Fatalf("rate=%g want %g", got, wantRate)
	}
	if got := m.FailureProbability([]int{-25, -40}, spec); math.Abs(got-want) > 1e-12 {
		t.Fatalf("resident profile changes loaded hazard: %g want %g", got, want)
	}
	if m.FailureProbability([]int{-30, 0}, spec) <= want || m.FailureProbability([]int{-20, 0}, spec) >= want {
		t.Fatal("hazard must be continuous and monotone in depth")
	}
	const draws = 10000
	failed := 0
	for i := range draws {
		cfg.Seed = uint64(i)
		m := newMachine(t, cfg)
		if err := m.Seams().SMU.SetOffset(0, -25); err != nil { t.Fatal(err) }
		r, err := m.Seams().Trials.Start(context.Background(), spec)
		if err != nil { t.Fatal(err) }
		result, err := r.Wait(context.Background(), nil)
		if err != nil { t.Fatal(err) }
		if result.Signal != "" { failed++ }
	}
	if got := float64(failed)/draws; math.Abs(got-want) > 0.02 {
		t.Fatalf("draw failure share=%g want %g", got, want)
	}
}

func TestSingleCoreValidation(t *testing.T) {
	for _, s := range []*SingleCore{
		{Core: []float64{0}},
		{Core: []float64{0, 0}, Slope: -1},
		{Core: []float64{math.NaN(), 0}},
		{Core: []float64{0, 0}, Workload: map[string]float64{"bad": math.Inf(1)}},
	} {
		if _, err := New(Config{Cores: 2, SingleCore: s}); err == nil { t.Fatalf("accepted invalid model %+v", s) }
	}
}
