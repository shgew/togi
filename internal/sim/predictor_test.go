package sim

import (
	"testing"
	"time"

	"github.com/shgew/togi/internal/machine"
)

func predictorConfig() Config {
	idle := -20
	model := DefaultModel()
	model.OnsetBoost, model.OnsetS = 3, 30
	limits := flat(4, -30, -25)
	limits[1].Flat = 1e-5
	limits[2].Idle = &idle
	limits[3].Workload = map[string]int{"custom": -12}
	return Config{
		Cores:  4,
		Limits: limits,
		Model:  &model,
		CCD:    &CCD{LogRate: -7, Slope: 0.05, Effect: [2]float64{0.5, -0.5}},
		Joints: []Joint{
			{Members: map[int]int{0: -20, 1: -22}, Regimes: []machine.Regime{machine.R7}, Rate: 0.01, AfterS: 20},
			{Members: map[int]int{2: -15, 3: -15}, Rate: 0.002},
		},
	}
}

func predictorTrials() (profiles [][]int, specs []machine.TrialSpec) {
	profiles = [][]int{{0, 0, 0, 0}, {-10, 0, 0, 0}, {-26, -26, -10, -10}, {-31, -23, -21, -16}, {-40, -40, -40, -40}, {0, 0, -17, -30}}
	for _, regime := range machine.Regimes {
		for _, cores := range [][]int{{0}, {2}, {0, 1}, {1, 3}, {0, 1, 2, 3}} {
			for _, workload := range []string{"", "custom"} {
				specs = append(specs, machine.TrialSpec{Regime: regime, Cores: cores, Workload: machine.Workload{ID: workload}, Duration: 90 * time.Second})
			}
		}
	}
	return profiles, specs
}

func TestPredictorMatchesMachine(t *testing.T) {
	cfg := predictorConfig()
	m := newMachine(t, cfg)
	p, err := NewPredictor(cfg)
	if err != nil {
		t.Fatal(err)
	}
	profiles, specs := predictorTrials()
	for _, profile := range profiles {
		for _, spec := range specs {
			if got, want := p.FailureProbability(profile, spec), m.FailureProbability(profile, spec); got != want {
				t.Fatalf("FailureProbability(%v, %+v) = %v, machine says %v", profile, spec, got, want)
			}
			if got, want := p.Hazard(profile, spec), m.Hazard(profile, spec); got != want {
				t.Fatalf("Hazard(%v, %+v) = %v, machine says %v", profile, spec, got, want)
			}
		}
	}
}

func TestPredictorOwnsItsConfig(t *testing.T) {
	cfg := predictorConfig()
	p, err := NewPredictor(cfg)
	if err != nil {
		t.Fatal(err)
	}
	profiles, specs := predictorTrials()
	before := make([]float64, 0, len(profiles)*len(specs))
	for _, profile := range profiles {
		for _, spec := range specs {
			before = append(before, p.FailureProbability(profile, spec))
		}
	}
	*cfg.Limits[2].Idle = -50
	cfg.Limits[3].Workload["custom"] = -50
	cfg.Limits[0].Flat = 0.5
	cfg.Joints[0].Members[0] = -50
	cfg.Joints[1].Rate = 0.5
	cfg.CCD.LogRate = 0
	cfg.Model.PastLimitRate = 0.5
	i := 0
	for _, profile := range profiles {
		for _, spec := range specs {
			if got := p.FailureProbability(profile, spec); got != before[i] {
				t.Fatalf("FailureProbability(%v, %+v) changed from %v to %v after the config changed", profile, spec, before[i], got)
			}
			i++
		}
	}
}

func TestNewPredictorRejects(t *testing.T) {
	invalid := predictorConfig()
	invalid.Joints[0].Rate = -1
	noLimits := predictorConfig()
	noLimits.Limits = nil
	badCore := predictorConfig()
	badCore.Joints[1].Members[9] = -10
	badOffset := predictorConfig()
	badOffset.Joints[1].Members[2] = 5
	for name, cfg := range map[string]Config{"negative joint rate": invalid, "no limits": noLimits, "member core out of range": badCore, "member offset out of range": badOffset} {
		if _, err := NewPredictor(cfg); err == nil {
			t.Errorf("%s: want an error", name)
		}
	}
}
