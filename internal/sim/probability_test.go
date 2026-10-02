package sim

import (
	"math"
	"testing"
	"time"

	"github.com/shgew/togi/internal/machine"
)

func TestFailureProbability(t *testing.T) {
	for _, tc := range []struct {
		name                                        string
		duration, after, onset, boost               float64
		loadedRate, idleRate, jointRate, wantHazard float64
	}{
		{"steady", 10, 0, 0, 0, 0.1, 0, 0, 1},
		{"negative-onset-boost", 10, 0, 10, -1, 0.1, 0, 0, 1},
		{"onset", 10, 0, 4, 2, 0.1, 0, 0, 1.8},
		{"within-onset", 2, 0, 4, 2, 0.1, 0, 0, 0.6},
		{"unloaded", 10, 0, 0, 0, 0.1, 0.2, 0, 3},
		{"delayed-joint", 10, 3, 4, 2, 0, 0, 0.1, 0.9},
		{"joint-after-end", 10, 12, 4, 2, 0, 0, 0.1, 0},
		{"joint-after-onset", 10, 6, 4, 2, 0, 0, 0.1, 0.4},
	} {
		t.Run(tc.name, func(t *testing.T) {
			model := DefaultModel()
			model.PastEdgeRate, model.NearEdgeRate = 0, 0
			model.OnsetS, model.OnsetBoost = tc.onset, tc.boost
			m, err := New(Config{Cores: 2, Model: &model, Edges: []Edges{{Flat: tc.loadedRate}, {Flat: tc.idleRate}}, Joints: []Joint{{Members: map[int]int{0: -10, 1: -10}, Rate: tc.jointRate, AfterS: tc.after}}})
			if err != nil {
				t.Fatal(err)
			}
			spec := machine.TrialSpec{Regime: machine.R1, Cores: []int{0}, Duration: time.Duration(tc.duration * float64(time.Second))}
			got := m.FailureProbability([]int{-10, -10}, spec)
			want := -math.Expm1(-tc.wantHazard)
			if math.Abs(got-want) > 1e-14 {
				t.Fatalf("got %.16g want %.16g", got, want)
			}
		})
	}
}
