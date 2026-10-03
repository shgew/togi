package sim

import (
	"context"
	"math"
	"testing"
	"time"

	"github.com/shgew/togi/internal/machine"
)

func TestSingleCoreResidentEffectAndDraws(t *testing.T) {
	model := DefaultModel()
	model.Signals = map[machine.Signal]float64{machine.ComputationError: 1}
	cfg := Config{Cores: 2, Edges: flat(2, -50, -50), Model: &model, SingleCore: &SingleCore{LogRate: math.Log(0.002), Slope: 0.2, ResidentEffect: math.Log(0.3), Core: []float64{0, 0}, ResidentCore: []float64{-0.3, 0.2}}}
	m := newMachine(t, cfg)
	spec := machine.TrialSpec{Regime: machine.R1, Cores: []int{0}, Condition: machine.Isolated, Duration: 90 * time.Second}
	isolated := m.FailureProbability([]int{-25, 0}, spec)
	resident := m.FailureProbability([]int{-25, -5}, spec)
	want := -math.Expm1(-0.002 * 0.3 * math.Exp(-0.3) * 90)
	if math.Abs(resident-want) > 1e-12 || !(resident < isolated) {
		t.Fatalf("isolated=%g resident=%g want=%g", isolated, resident, want)
	}
	spec.Condition = machine.Resident
	if got := m.FailureProbability([]int{-25, 0}, spec); got != isolated {
		t.Fatal("condition label, rather than applied registers, changes rate")
	}
	const draws = 10000
	failed := 0
	for i := range draws {
		cfg.Seed = uint64(i)
		m := newMachine(t, cfg)
		if err := m.Seams().SMU.SetAllOffsets(-25); err != nil {
			t.Fatal(err)
		}
		r, err := m.Seams().Trials.Start(context.Background(), spec)
		if err != nil {
			t.Fatal(err)
		}
		result, err := r.Wait(context.Background(), nil)
		if err != nil {
			t.Fatal(err)
		}
		if result.Signal != "" {
			failed++
		}
	}
	if got := float64(failed) / draws; math.Abs(got-want) > 0.02 {
		t.Fatalf("draws=%g want=%g", got, want)
	}
}

func TestSingleCoreValidation(t *testing.T) {
	for _, s := range []*SingleCore{{Core: []float64{0}}, {Core: []float64{0, 0}, Slope: -1}, {Core: []float64{0, 0}, ResidentEffect: math.NaN()}, {Core: []float64{math.Inf(1), 0}}, {Core: []float64{0, 0}, ResidentCore: []float64{0}}, {Core: []float64{0, 0}, ResidentCore: []float64{math.NaN(), 0}}} {
		if _, err := New(Config{Cores: 2, SingleCore: s}); err == nil {
			t.Fatalf("accepted %+v", s)
		}
	}
}
