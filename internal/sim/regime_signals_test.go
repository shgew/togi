package sim

import (
	"errors"
	"math"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func TestRegimeSignalsReplaceModelSignalsInTheirRegime(t *testing.T) {
	t.Parallel()
	model := sharp(machine.Crash)
	model.RegimeSignals = map[machine.Regime]map[machine.Signal]float64{machine.R2: {machine.ComputationError: 1}}
	m := newMachine(t, Config{Seed: 1, Cores: 2, Limits: flat(2, -10, -10), Model: model})
	res, err := trial(m, 0, -12, machine.R2, machine.Alone, 0)
	if err != nil || res.Signal != machine.ComputationError || res.Core != 0 {
		t.Fatalf("R2 result %+v, %v; want computation_error on core 0", res, err)
	}
	if _, err := trial(m, 0, -12, machine.R1, machine.Alone, 1); !errors.Is(err, machine.ErrCrashed) {
		t.Fatalf("R1 error %v; want the model.signals crash", err)
	}
}

func TestRegimeSignalsDrawUnattributedR7Failures(t *testing.T) {
	t.Parallel()
	computation := map[machine.Regime]map[machine.Signal]float64{machine.R7: {machine.ComputationError: 1}}
	otherRegime := map[machine.Regime]map[machine.Signal]float64{machine.R2: {machine.ComputationError: 1}}
	for _, tc := range []struct {
		name    string
		cfg     Config
		regimes map[machine.Regime]map[machine.Signal]float64
		loaded  []int
		want    machine.Result
	}{
		{
			name:    "joint without signal draws the regime mix",
			cfg:     Config{Cores: 2, Joints: []Joint{{Members: map[int]int{0: -20, 1: -20}, Rate: 1e6}}},
			regimes: computation,
			loaded:  []int{1, 0},
			want:    machine.Result{Signal: machine.ComputationError, Core: 1},
		},
		{
			name:   "joint without signal or regime mix crashes",
			cfg:    Config{Cores: 2, Joints: []Joint{{Members: map[int]int{0: -20, 1: -20}, Rate: 1e6}}},
			loaded: []int{1, 0},
		},
		{
			name:    "joint without signal in an unlisted regime draws the pooled mix",
			cfg:     Config{Cores: 2, Joints: []Joint{{Members: map[int]int{0: -20, 1: -20}, Rate: 1e6}}},
			regimes: otherRegime,
			loaded:  []int{1, 0},
			want:    machine.Result{Signal: machine.UnexpectedExit, Core: 1},
		},
		{
			name:    "joint with a signal keeps it",
			cfg:     Config{Cores: 2, Joints: []Joint{{Members: map[int]int{0: -20, 1: -20}, Rate: 1e6, Signal: machine.Stall}}},
			regimes: computation,
			loaded:  []int{0, 1},
			want:    machine.Result{Signal: machine.Stall, Core: 0},
		},
		{
			name:    "joint on unloaded cores crashes",
			cfg:     Config{Cores: 4, Joints: []Joint{{Members: map[int]int{2: -20, 3: -20}, Rate: 1e6}}},
			regimes: computation,
			loaded:  []int{0},
		},
		{
			name:    "joint on unloaded cores in an unlisted regime crashes",
			cfg:     Config{Cores: 4, Joints: []Joint{{Members: map[int]int{2: -20, 3: -20}, Rate: 1e6}}},
			regimes: otherRegime,
			loaded:  []int{0},
		},
		{
			name:    "ccd hazard draws the regime mix",
			cfg:     Config{Cores: 2, CCD: &CCD{LogRate: math.Log(1e6)}},
			regimes: computation,
			loaded:  []int{1},
			want:    machine.Result{Signal: machine.ComputationError, Core: 1},
		},
		{
			name:   "ccd hazard without regime mix crashes",
			cfg:    Config{Cores: 2, CCD: &CCD{LogRate: math.Log(1e6)}},
			loaded: []int{1},
		},
		{
			name:    "ccd hazard in an unlisted regime draws the pooled mix",
			cfg:     Config{Cores: 2, CCD: &CCD{LogRate: math.Log(1e6)}},
			regimes: otherRegime,
			loaded:  []int{1},
			want:    machine.Result{Signal: machine.UnexpectedExit, Core: 1},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			model := DefaultModel()
			model.Signals = map[machine.Signal]float64{machine.UnexpectedExit: 1}
			model.RegimeSignals = tc.regimes
			cfg := tc.cfg
			cfg.Limits, cfg.Model = flat(cfg.Cores, -50, -50), &model
			for j := range cfg.Joints {
				cfg.Joints[j].Regimes = []machine.Regime{machine.R7}
			}
			m := newMachine(t, cfg)
			if err := m.Seams().SMU.SetAllOffsets(-20); err != nil {
				t.Fatal(err)
			}
			res, err := runSpec(t, m, "0001", machine.R7, machine.PickWorkload(machine.R7, 0), tc.loaded, time.Minute, nil)
			if tc.want.Signal == "" {
				if !errors.Is(err, machine.ErrCrashed) {
					t.Fatalf("result %+v, %v; want a crash", res, err)
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			got := machine.Result{Signal: res.Signal, Core: res.Core}
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Fatalf("result (-want +got):\n%s", diff)
			}
		})
	}
}
