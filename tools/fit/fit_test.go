package main

import (
	"maps"
	"math"
	"math/rand/v2"
	"slices"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/tools/modelcheck"
	"github.com/shgew/togi/tools/trialfacts"
)

func synthetic(t *testing.T) (sim.Config, []trialfacts.Record) {
	t.Helper()
	model := sim.DefaultModel()
	model.PastEdgeRate, model.Growth = 0.008, 3
	cfg := sim.Config{Cores: 4, Model: &model, Edges: make([]sim.Edges, 4), Joints: []sim.Joint{
		{Members: map[int]int{0: -25, 1: -27}, Regimes: []machine.Regime{machine.R7}, Rate: 0.006},
		{Members: map[int]int{2: -33, 3: -34}, Regimes: []machine.Regime{machine.R7}, Rate: 0.012},
	}}
	for core := range cfg.Edges {
		for r := range cfg.Edges[core].Isolated {
			cfg.Edges[core].Isolated[r] = -20 - core
		}
		for r := range cfg.Edges[core].Resident {
			cfg.Edges[core].Resident[r] = -18 - core
		}
		cfg.Edges[core].Resident[6] = -50
	}
	m, err := sim.New(cfg)
	if err != nil {
		t.Fatal(err)
	}
	rng := rand.New(rand.NewPCG(12, 34))
	var records []trialfacts.Record
	add := func(profile []int, regime machine.Regime, cores []int, repetitions int) {
		spec := machine.TrialSpec{Regime: regime, Workload: machine.Workload{ID: "synthetic"}, Cores: cores, Duration: 60 * time.Second}
		p := m.FailureProbability(profile, spec)
		for range repetitions {
			outcome := journal.OutcomePass
			if rng.Float64() < p {
				outcome = journal.OutcomeFailure
			}
			records = append(records, trialfacts.Record{Session: "synthetic", Seq: len(records) + 1, Kind: facts.TrialFact, Class: facts.Class{Regime: regime, Workload: "synthetic", Cores: cores, DurationS: 60}, Profile: slices.Clone(profile), Outcome: outcome})
		}
	}
	for core := range cfg.Edges {
		for _, regime := range []machine.Regime{machine.R1, machine.R2} {
			for _, isolated := range []bool{true, false} {
				edge := cfg.Edges[core].Resident[0]
				if isolated {
					edge = cfg.Edges[core].Isolated[0]
				}
				for delta := -1; delta <= 3; delta++ {
					profile := make([]int, 4)
					if !isolated {
						profile[(core+1)%4] = -5
					}
					profile[core] = edge - delta
					add(profile, regime, []int{core}, 60)
				}
			}
		}
	}
	for _, joint := range cfg.Joints {
		cores := slices.Sorted(maps.Keys(joint.Members))
		for a := -1; a <= 1; a++ {
			for b := -1; b <= 1; b++ {
				profile := make([]int, 4)
				profile[cores[0]] = joint.Members[cores[0]] + a
				profile[cores[1]] = joint.Members[cores[1]] + b
				add(profile, machine.R7, cores, 60)
			}
		}
	}
	return cfg, records
}

func TestFitRecoversMachine(t *testing.T) {
	known, records := synthetic(t)
	got, _ := fit(records)
	for core := range known.Edges {
		for r := range 2 {
			if got.Edges[core].Isolated[r] != known.Edges[core].Isolated[r] || got.Edges[core].Resident[r] != known.Edges[core].Resident[r] {
				t.Errorf("core %d regime %d: edges %+v; want %+v", core, r, got.Edges[core], known.Edges[core])
			}
		}
	}
	if math.Abs(got.Model.PastEdgeRate/known.Model.PastEdgeRate-1) > 0.25 || math.Abs(got.Model.Growth-known.Model.Growth) > 0.6 {
		t.Errorf("hazard shape: rate=%g growth=%g; want rate=%g growth=%g", got.Model.PastEdgeRate, got.Model.Growth, known.Model.PastEdgeRate, known.Model.Growth)
	}
	fitted, err := sim.New(got)
	if err != nil {
		t.Fatal(err)
	}
	source, err := sim.New(known)
	if err != nil {
		t.Fatal(err)
	}
	for _, joint := range known.Joints {
		cores := slices.Sorted(maps.Keys(joint.Members))
		for a := -1; a <= 1; a++ {
			for b := -1; b <= 1; b++ {
				profile := make([]int, known.Cores)
				profile[cores[0]] = joint.Members[cores[0]] + a
				profile[cores[1]] = joint.Members[cores[1]] + b
				spec := machine.TrialSpec{Regime: machine.R7, Workload: machine.Workload{ID: "synthetic"}, Cores: cores, Duration: 60 * time.Second}
				want, actual := source.FailureProbability(profile, spec), fitted.FailureProbability(profile, spec)
				if math.Abs(actual-want) > 0.12 {
					t.Errorf("joint profile %v: p=%g want %g", profile, actual, want)
				}
			}
		}
	}
}

func TestBootstrapDeterminism(t *testing.T) {
	_, records := synthetic(t)
	a, b := bootstrap(records, 263), bootstrap(records, 263)
	if diff := cmp.Diff(a, b); diff != "" {
		t.Fatal(diff)
	}
	first, lossA := fit(a)
	second, lossB := fit(b)
	if diff := cmp.Diff(encodeMachine(first, 1, 263, len(a), lossA, nil), encodeMachine(second, 1, 263, len(b), lossB, nil)); diff != "" {
		t.Fatal(diff)
	}
	if cmp.Equal(a, bootstrap(records, 264)) {
		t.Fatal("distinct seeds produced the same resampled trials")
	}
}

func TestMixedContextRejected(t *testing.T) {
	r := trialfacts.Record{Kind: facts.TrialFact, Outcome: journal.OutcomePass, Class: facts.Class{Regime: machine.R1, Cores: []int{0}, DurationS: 90}, Profile: []int{0, 0}, Context: &machine.BIOSContext{BIOSVersion: "A"}}
	s := r
	s.Context = &machine.BIOSContext{BIOSVersion: "B"}
	if _, err := decisive([]trialfacts.Record{r, s}); err == nil {
		t.Fatal("mixed BIOS evidence accepted")
	}
}

func TestFitRecoversFlatAndIdle(t *testing.T) {
	model := sim.DefaultModel()
	model.PastEdgeRate, model.Growth = 0.008, 3
	idle := -30
	known := sim.Config{Cores: 2, Model: &model, Edges: []sim.Edges{
		{Isolated: [5]int{-50, -50, -50, -50, -50}, Resident: [7]int{-50, -50, -50, -50, -50, -50, -50}, Flat: 0.00015},
		{Isolated: [5]int{-50, -50, -50, -50, -50}, Resident: [7]int{-50, -50, -50, -50, -50, -50, -50}, Idle: &idle},
	}}
	source, err := sim.New(known)
	if err != nil {
		t.Fatal(err)
	}
	rng := rand.New(rand.NewPCG(56, 78))
	var records []trialfacts.Record
	for _, exposure := range []struct {
		profile  []int
		loaded   int
		duration int
	}{
		{[]int{0, 0}, 0, 600},
		{[]int{-5, 0}, 0, 600},
		{[]int{-15, 0}, 0, 600},
		{[]int{-25, 0}, 0, 600},
		{[]int{-5, -20}, 1, 600},
		{[]int{0, -20}, 0, 600},
		{[]int{0, -30}, 0, 60},
		{[]int{0, -31}, 0, 60},
		{[]int{0, -32}, 0, 60},
		{[]int{0, -33}, 0, 60},
	} {
		cores := []int{exposure.loaded}
		spec := machine.TrialSpec{Regime: machine.R1, Cores: cores, Duration: time.Duration(exposure.duration) * time.Second}
		p := source.FailureProbability(exposure.profile, spec)
		for range 150 {
			outcome := journal.OutcomePass
			if rng.Float64() < p {
				outcome = journal.OutcomeFailure
			}
			records = append(records, trialfacts.Record{Kind: facts.TrialFact, Profile: exposure.profile, Class: facts.Class{Regime: machine.R1, Cores: cores, DurationS: exposure.duration}, Outcome: outcome})
		}
	}
	got, _ := fit(records)
	if got.Edges[1].Idle == nil {
		t.Errorf("idle edge: disabled; want %d", idle)
	} else if *got.Edges[1].Idle != idle {
		t.Errorf("idle edge: got %d want %d (past_edge_rate=%g growth=%g)", *got.Edges[1].Idle, idle, got.Model.PastEdgeRate, got.Model.Growth)
	}
	if math.Abs(got.Edges[0].Flat/known.Edges[0].Flat-1) > 0.4 {
		t.Errorf("flat rate: got %g want %g", got.Edges[0].Flat, known.Edges[0].Flat)
	}
}

func TestConstrainedFitUsesResampledLikelihood(t *testing.T) {
	model := sim.DefaultModel()
	base := sim.Config{Cores: 2, Model: &model, Edges: []sim.Edges{
		{Isolated: [5]int{-50, -50, -50, -50, -50}, Resident: [7]int{-50, -50, -50, -50, -50, -50, -50}, Flat: -math.Log(0.8) / 900},
		{Isolated: [5]int{-50, -50, -50, -50, -50}, Resident: [7]int{-50, -50, -50, -50, -50, -50, -50}},
	}}
	var original, sample []trialfacts.Record
	for i := range 40 {
		r := trialfacts.Record{Kind: facts.TrialFact, Profile: []int{-10, 0}, Class: facts.Class{Regime: machine.R1, Cores: []int{0}, DurationS: 900}, Outcome: journal.OutcomePass}
		if i < 8 {
			r.Outcome = journal.OutcomeFailure
		}
		original = append(original, r)
		if i < 32 {
			r.Outcome = journal.OutcomeFailure
		}
		sample = append(sample, r)
	}
	checker, err := modelcheck.NewChecker(base, original)
	if err != nil {
		t.Fatal(err)
	}
	fitted, _ := fitFrom(sample, &base, checker)
	m, err := sim.New(fitted)
	if err != nil {
		t.Fatal(err)
	}
	check := checker.Check("synthetic", "synthetic", m)
	if check.Status != "ok" || check.Groups[0].MeanP <= 0.25 {
		t.Fatalf("resampled likelihood must increase failure probability without violating original evidence: %+v", check)
	}
	if base.Edges[0].Flat != -math.Log(0.8)/900 {
		t.Fatal("constrained refit mutated the all-facts starting machine")
	}
}

func TestLikelihoodRejectsImpossibleOutcomes(t *testing.T) {
	cfg := sim.Config{Cores: 2, Edges: []sim.Edges{
		{Isolated: [5]int{-50, -50, -50, -50, -50}, Resident: [7]int{-50, -50, -50, -50, -50, -50, -50}},
		{Isolated: [5]int{-50, -50, -50, -50, -50}, Resident: [7]int{-50, -50, -50, -50, -50, -50, -50}},
	}}
	l := likelihood{cfg: cfg, obs: []observation{{profile: []int{-10, 0}, spec: machine.TrialSpec{Regime: machine.R1, Cores: []int{0}, Duration: 90 * time.Second}, n: 1}}}
	l.rebuild()
	for _, tc := range []struct {
		flat       float64
		failures   int
		impossible bool
	}{{0, 1, true}, {100, 0, true}, {0, 0, false}, {100, 1, false}} {
		cfg.Edges[0].Flat, l.obs[0].k = tc.flat, tc.failures
		loss := l.value(0)
		if tc.impossible {
			if !math.IsInf(loss, 1) {
				t.Errorf("flat=%g failures=%d: impossible outcome has finite loss %g", tc.flat, tc.failures, loss)
			}
		} else if loss != 0 {
			t.Errorf("flat=%g failures=%d: certain outcome has loss %g", tc.flat, tc.failures, loss)
		}
	}
}
