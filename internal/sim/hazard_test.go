package sim

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func TestHazard(t *testing.T) {
	model := DefaultModel()
	model.PastLimitRate, model.Growth, model.NearLimitRate = 2, 4, 0.25
	model.OnsetBoost = 100
	limits := []Limits{
		{Alone: [5]int{-10, -10, -10, -10, -10}, Together: [7]int{-12, -12, -12, -12, -12, -12, -12}},
		{Alone: [5]int{-10, -10, -10, -10, -10}, Together: [7]int{-12, -12, -12, -12, -12, -12, -12}},
	}
	for _, tc := range []struct {
		name     string
		profile  []int
		cores    []int
		regime   machine.Regime
		flat     float64
		idle     *int
		workload string
		joint    Joint
		want     float64
	}{
		{name: "past alone limit", profile: []int{-12, 0}, cores: []int{0}, regime: machine.R1, want: 8},
		{name: "near limit", profile: []int{-10, 0}, cores: []int{0}, regime: machine.R1, want: 0.25},
		{name: "together limit", profile: []int{-12, -1}, cores: []int{0}, regime: machine.R1, want: 0.25},
		{name: "flat nonzero unloaded", profile: []int{-1, 0}, regime: machine.R1, flat: 0.5, want: 0.5},
		{name: "flat zero unloaded", profile: []int{0, 0}, regime: machine.R1, flat: 0.5, want: 0},
		{name: "flat loaded zero", profile: []int{0, 0}, cores: []int{0}, regime: machine.R1, flat: 0.5, want: 0.25},
		{name: "idle limit", profile: []int{-9, 0}, regime: machine.R1, idle: new(-8), want: 2},
		{name: "workload limit", profile: []int{-9, 0}, cores: []int{0}, regime: machine.R1, workload: "custom", want: 2},
		{name: "active joint ignores delay", profile: []int{-5, -5}, regime: machine.R7, joint: Joint{Members: map[int]int{0: -5, 1: -5}, Regimes: []machine.Regime{machine.R7}, Rate: 3, AfterS: 240}, want: 3},
		{name: "inactive joint", profile: []int{-5, -4}, regime: machine.R7, joint: Joint{Members: map[int]int{0: -5, 1: -5}, Rate: 3}, want: 0},
		{name: "joint regime filter", profile: []int{-5, -5}, regime: machine.R1, joint: Joint{Members: map[int]int{0: -5}, Regimes: []machine.Regime{machine.R7}, Rate: 3}, want: 0},
		{name: "joint default rate", profile: []int{-5, -5}, regime: machine.R7, joint: Joint{Members: map[int]int{0: -5}}, want: 2},
	} {
		t.Run(tc.name, func(t *testing.T) {
			e := append([]Limits(nil), limits...)
			e[0].Flat, e[0].Idle = tc.flat, tc.idle
			e[0].Workload = map[string]int{"custom": -8}
			m, err := New(Config{Cores: 2, Limits: e, Model: &model, Joints: []Joint{tc.joint}})
			if err != nil {
				t.Fatal(err)
			}
			got := m.Hazard(tc.profile, machine.TrialSpec{Cores: tc.cores, Regime: tc.regime, Workload: machine.Workload{ID: tc.workload}})
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Fatalf("hazard (-want +got):\n%s", diff)
			}
		})
	}
}
