package sim

import (
	"context"
	"errors"
	"math"
	"testing"
	"time"

	"github.com/shgew/togi/internal/machine"
)

func TestCCDLoadedHazardAndDraws(t *testing.T) {
	cfg := Config{Cores: 4, Edges: flat(4, -50, -50), CCD: &CCD{LogRate: math.Log(0.002), Slope: 0.1, Effect: [2]float64{0, 1}}}
	m := newMachine(t, cfg)
	spec := machine.TrialSpec{Regime: machine.R7, Cores: []int{0, 1}, Duration: 90 * time.Second}
	profile := []int{-25, -25, -40, -40}
	want := -math.Expm1(-0.002 * 90)
	if got := m.FailureProbability(profile, spec); math.Abs(got-want) > 1e-12 {
		t.Fatalf("p=%g want %g", got, want)
	}
	if got := m.FailureProbability([]int{-25, -25, 0, 0}, spec); math.Abs(got-want) > 1e-12 {
		t.Fatalf("unloaded CCD adds hazard: %g", got)
	}
	if got := m.Hazard(profile, spec); math.Abs(got-0.002) > 1e-12 {
		t.Fatalf("rate=%g", got)
	}
	if m.FailureProbability([]int{-30, -30, 0, 0}, spec) <= want {
		t.Fatal("deeper CCD must be riskier")
	}
	spec.Cores = []int{0, 2}
	if m.FailureProbability(profile, spec) <= want {
		t.Fatal("both loaded CCDs must contribute")
	}
	spec.Regime = machine.R1
	if m.FailureProbability(profile, spec) != 0 {
		t.Fatal("CCD hazard applies only to R7")
	}
	spec.Regime, spec.Cores = machine.R7, []int{0, 1}
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
		_, err = r.Wait(context.Background(), nil)
		if errors.Is(err, machine.ErrCrashed) {
			failed++
		} else if err != nil {
			t.Fatal(err)
		}
	}
	if got := float64(failed) / draws; math.Abs(got-want) > 0.02 {
		t.Fatalf("draw share=%g want %g", got, want)
	}
}

func TestCCDValidation(t *testing.T) {
	for _, c := range []*CCD{{Slope: -1}, {LogRate: math.Inf(1)}, {Effect: [2]float64{math.NaN(), 0}}} {
		if _, err := New(Config{Cores: 2, CCD: c}); err == nil {
			t.Fatalf("accepted %+v", c)
		}
	}
}

func TestCCDResidualDefersToExistingJoint(t *testing.T) {
	cfg := Config{Cores: 4, Edges: flat(4, -50, -50), CCD: &CCD{LogRate: math.Log(0.002)}, Joints: []Joint{{Members: map[int]int{0: -20, 1: -20}, Regimes: []machine.Regime{machine.R7}, Rate: 0.01}}}
	m := newMachine(t, cfg)
	spec := machine.TrialSpec{Regime: machine.R7, Cores: []int{0, 1, 2, 3}, Duration: time.Minute}
	profile := []int{-25, -25, -25, -25}
	if got := m.ccdRate(profile, spec, 0); got != 0 {
		t.Fatalf("active joint must suppress residual rate: %g", got)
	}
	if got := m.ccdRate(profile, spec, 1); math.Abs(got-0.002) > 1e-12 {
		t.Fatalf("other CCD residual rate=%g", got)
	}
	if got := m.Hazard(profile, spec); math.Abs(got-0.012) > 1e-12 {
		t.Fatalf("joint plus uncovered CCD rate=%g", got)
	}
	if got := m.ccdRate([]int{-19, -25, -25, -25}, spec, 0); math.Abs(got-0.002) > 1e-12 {
		t.Fatalf("inactive joint suppressed residual: %g", got)
	}
}
