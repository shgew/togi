package main

import (
	"testing"
	"time"

	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
)

func TestFitIdlePreservesTotalLikelihood(t *testing.T) {
	model := sim.DefaultModel()
	model.PastLimitRate, model.Growth = 0.015, 2
	cfg := sim.Config{Cores: 2, Model: &model, Limits: []sim.Limits{
		{Alone: [5]int{-50, -50, -50, -50, -50}, Together: [7]int{-50, -50, -50, -50, -50, -50, -50}, Flat: 0.001},
		{Alone: [5]int{-50, -50, -50, -50, -50}, Together: [7]int{-50, -50, -50, -50, -50, -50, -50}},
	}}
	spec := machine.TrialSpec{Regime: machine.R1, Cores: []int{0}, Duration: 60 * time.Second}
	l := likelihood{cfg: cfg, obs: []observation{
		{profile: []int{-1, -1}, spec: spec, n: 1, k: 1},
		{profile: []int{-1, 0}, spec: spec, n: 100},
	}}
	l.rebuild()
	all := l.selectObs(func(observation) bool { return true })
	before := l.score(all)
	l.fitIdle()
	if got := l.cfg.Limits[1].Idle; got == nil {
		t.Error("idle limit disabled; want 0")
	} else if *got != 0 {
		t.Errorf("idle limit: got %d; want 0 to explain the negative-offset failure without adding stock-offset hazards", *got)
	}
	if after := l.score(all); after > before+1e-9 {
		t.Errorf("idle coordinate worsened total negative log likelihood: before=%g after=%g", before, after)
	}
}

func TestFitIdleRequiresNegativeUnloadedExposure(t *testing.T) {
	for _, tc := range []struct {
		name        string
		enabled     bool
		initial     int
		wantEnabled bool
	}{
		{name: "disabled"},
		{name: "existing limit", enabled: true, initial: 0, wantEnabled: true},
		{name: "unsupported limit", enabled: true, initial: -50},
	} {
		t.Run(tc.name, func(t *testing.T) {
			model := sim.DefaultModel()
			model.PastLimitRate, model.Growth = 0.015, 2
			cfg := sim.Config{Cores: 2, Model: &model, Limits: []sim.Limits{
				{Alone: [5]int{-50, -50, -50, -50, -50}, Together: [7]int{-50, -50, -50, -50, -50, -50, -50}, Flat: 0.001},
				{Alone: [5]int{-50, -50, -50, -50, -50}, Together: [7]int{-50, -50, -50, -50, -50, -50, -50}},
			}}
			if tc.enabled {
				idle := tc.initial
				cfg.Limits[1].Idle = &idle
			}
			l := likelihood{cfg: cfg, obs: []observation{
				{
					profile: []int{-1, 0},
					spec:    machine.TrialSpec{Regime: machine.R1, Cores: []int{0}, Duration: 60 * time.Second},
					n:       1,
					k:       1,
				},
				{
					profile: []int{0, -1},
					spec:    machine.TrialSpec{Regime: machine.R1, Cores: []int{1}, Duration: 60 * time.Second},
					n:       100,
				},
			}}
			l.rebuild()
			l.fitIdle()
			got := l.cfg.Limits[1].Idle
			switch {
			case !tc.wantEnabled:
				if got != nil {
					t.Errorf("idle limit without negative unloaded exposure: got %d; want disabled", *got)
				}
			case got == nil:
				t.Errorf("existing idle limit disabled without negative unloaded exposure; want %d", tc.initial)
			case *got != tc.initial:
				t.Errorf("existing idle limit without negative unloaded exposure: got %d; want %d", *got, tc.initial)
			}
		})
	}
}
