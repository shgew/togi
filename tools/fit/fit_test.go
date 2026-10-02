package main

import (
	"maps"
	"math"
	"math/rand/v2"
	"slices"
	"strconv"
	"strings"
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

func TestDecisiveRefusesInvalidEvidence(t *testing.T) {
	valid := trialfacts.Record{Session: "extract", Seq: 7, Kind: facts.TrialFact, Outcome: journal.OutcomePass, Profile: []int{-50, 0}, Class: facts.Class{Regime: machine.R1, Cores: []int{0}, DurationS: 60}}
	for _, tc := range []struct {
		name   string
		change func(*trialfacts.Record)
		want   string
	}{
		{"one core", func(r *trialfacts.Record) { r.Profile = []int{0} }, "invalid trial fact extract:7"},
		{"odd cores", func(r *trialfacts.Record) { r.Profile = []int{0, 0, 0} }, "invalid trial fact extract:7"},
		{"no loaded cores", func(r *trialfacts.Record) { r.Class.Cores = nil }, "invalid trial fact extract:7"},
		{"no exposure", func(r *trialfacts.Record) { r.Class.DurationS = 0 }, "invalid trial fact extract:7"},
		{"negative exposure", func(r *trialfacts.Record) { r.Class.DurationS = -1 }, "invalid trial fact extract:7"},
		{"unknown regime", func(r *trialfacts.Record) { r.Class.Regime = "unknown" }, "invalid trial fact extract:7"},
		{"too deep", func(r *trialfacts.Record) { r.Profile = []int{-51, 0} }, "invalid profile in fact extract:7"},
		{"positive offset", func(r *trialfacts.Record) { r.Profile = []int{1, 0} }, "invalid profile in fact extract:7"},
		{"negative core", func(r *trialfacts.Record) { r.Class.Cores = []int{-1} }, "invalid loaded core in fact extract:7"},
		{"past last core", func(r *trialfacts.Record) { r.Class.Cores = []int{2} }, "invalid loaded core in fact extract:7"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			r := valid
			tc.change(&r)
			got, err := decisive([]trialfacts.Record{r})
			if err == nil || err.Error() != tc.want || got != nil {
				t.Fatalf("got %v, %v; want %q", got, err, tc.want)
			}
		})
	}
	mismatch := valid
	mismatch.Profile = []int{0, 0, 0, 0}
	if _, err := decisive([]trialfacts.Record{valid, mismatch}); err == nil || !strings.Contains(err.Error(), "invalid trial fact") {
		t.Fatalf("inconsistent core count: %v", err)
	}
}

func TestDecisiveFiltersNonStartsAndPreservesContext(t *testing.T) {
	context := machine.BIOSContext{Board: "fixture", BIOSVersion: "A"}
	valid := trialfacts.Record{Kind: facts.TrialFact, Outcome: journal.OutcomeFailure, Profile: []int{-50, 0}, Class: facts.Class{Regime: machine.R1, Cores: []int{0}, DurationS: 60}, Context: &context}
	idle := valid
	idle.Kind = facts.IdleFact
	other := valid
	other.Outcome = journal.Outcome("inconclusive")
	got, err := decisive([]trialfacts.Record{idle, other, valid})
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]trialfacts.Record{valid}, got); diff != "" {
		t.Fatal(diff)
	}
	cfg := initialConfig(got)
	if diff := cmp.Diff(context, cfg.BIOSContext); diff != "" {
		t.Fatal(diff)
	}
	for _, records := range [][]trialfacts.Record{nil, {idle, other}} {
		if got, err := decisive(records); err == nil || err.Error() != "extract has no decisive starts" || got != nil {
			t.Fatalf("empty evidence: %v %v", got, err)
		}
	}
	missing := valid
	missing.Context = nil
	for _, records := range [][]trialfacts.Record{{valid, missing}, {missing, valid}} {
		if _, err := decisive(records); err == nil || !strings.Contains(err.Error(), "mixed BIOS contexts") {
			t.Fatalf("missing context mixed with known: %v", err)
		}
	}
}

func TestCloneMachineIsolatesConstrainedParameters(t *testing.T) {
	idle := -20
	model := sim.DefaultModel()
	cfg := sim.Config{Cores: 2, Model: &model, Edges: []sim.Edges{{Idle: &idle, Workload: map[string]int{"work": -25}}, {}}, Joints: []sim.Joint{{Members: map[int]int{0: -30}, Rate: 0.01}}}
	wantModel := model
	wantIdle := -20
	want := sim.Config{Cores: 2, Model: &wantModel, Edges: []sim.Edges{{Idle: &wantIdle, Workload: map[string]int{"work": -25}}, {}}, Joints: []sim.Joint{{Members: map[int]int{0: -30}, Rate: 0.01}}}
	got := cloneMachine(cfg)
	got.Model.PastEdgeRate = 0.4
	*got.Edges[0].Idle = -1
	got.Edges[0].Workload["work"] = -1
	got.Joints[0].Members[0] = -1
	got.Joints[0].Rate = 0.2
	if diff := cmp.Diff(want, cfg); diff != "" {
		t.Fatalf("refit changed its seed: %s", diff)
	}
}

func TestJointSearchSeparatesCleanBoundary(t *testing.T) {
	cfg := initialConfig([]trialfacts.Record{{Profile: []int{0, 0}}})
	cfg.Model.NearEdgeRate = 0
	cfg.Joints = []sim.Joint{{Members: map[int]int{0: -10, 1: -10}, Regimes: []machine.Regime{machine.R7}, Rate: 0.01}}
	spec := machine.TrialSpec{Regime: machine.R7, Cores: []int{0, 1}, Duration: 60 * time.Second}
	l := likelihood{cfg: cfg, obs: []observation{{profile: []int{-20, -20}, spec: spec, n: 100}, {profile: []int{-30, -30}, spec: spec, n: 100, k: 50}}}
	l.rebuild()
	all := []int{0, 1}
	before := l.score(all)
	l.fitJoints(&cfg)
	l.fitJoints(&cfg)
	if after := l.score(all); !(after < before-1) {
		t.Fatalf("joint did not improve likelihood: %g -> %g", before, after)
	}
	if p := l.m.FailureProbability([]int{-20, -20}, spec); p != 0 {
		t.Fatalf("clean boundary retains joint hazard: %g", p)
	}
	if p := l.m.FailureProbability([]int{-30, -30}, spec); math.Abs(p-0.5) > 0.01 {
		t.Fatalf("failure boundary p=%g; want 0.5", p)
	}
}

func TestJointCandidateLimitPreservesModel(t *testing.T) {
	cfg := initialConfig([]trialfacts.Record{{Profile: []int{0, 0}}})
	for j := range 8 {
		cfg.Joints = append(cfg.Joints, sim.Joint{Members: map[int]int{0: -20 - j}, Regimes: []machine.Regime{machine.R7}, Rate: 0.001})
	}
	want := cloneMachine(cfg)
	l := likelihood{cfg: cfg}
	l.rebuild()
	l.addJoint(&cfg, []trialfacts.Record{{Kind: facts.TrialFact, Outcome: journal.OutcomeFailure, Profile: []int{-30, 0}, Class: facts.Class{Regime: machine.R7, Cores: []int{0}, DurationS: 60}}}, 0)
	if diff := cmp.Diff(want, cfg); diff != "" {
		t.Fatalf("ninth joint admitted: %s", diff)
	}
}

func TestWorkloadOverridesRequireSupportAndImproveLikelihood(t *testing.T) {
	for _, n := range []int{9, 10} {
		t.Run(strconv.Itoa(n), func(t *testing.T) {
			cfg := initialConfig([]trialfacts.Record{{Profile: []int{0, 0}}})
			cfg.Model.NearEdgeRate = 0
			spec := machine.TrialSpec{Regime: machine.R1, Workload: machine.Workload{ID: "supported"}, Cores: []int{0}, Duration: 60 * time.Second}
			l := likelihood{cfg: cfg, obs: []observation{{profile: []int{-20, 0}, spec: spec, n: n, k: 1}}}
			l.rebuild()
			before := l.score([]int{0})
			l.fitWorkloads(&cfg)
			_, exists := cfg.Edges[0].Workload["supported"]
			if exists != (n >= 10) {
				t.Fatalf("%d starts: override=%v", n, exists)
			}
			if n >= 10 && !(l.score([]int{0}) < before) {
				t.Fatal("supported override did not explain failure")
			}
		})
	}
}

func TestCoupledShiftIncludesWorkloadAndPreservesUnsupportedEdges(t *testing.T) {
	cfg := initialConfig([]trialfacts.Record{{Profile: []int{0, 0}}})
	cfg.Model.NearEdgeRate = 0
	cfg.Model.PastEdgeRate = 0.01
	cfg.Edges[0].Workload = map[string]int{"active": -20, "unsupported": -50}
	spec := machine.TrialSpec{Regime: machine.R1, Workload: machine.Workload{ID: "active"}, Cores: []int{0}, Duration: 60 * time.Second}
	l := likelihood{cfg: cfg, obs: []observation{{profile: []int{-21, 0}, spec: spec, n: 100}, {profile: []int{-24, 0}, spec: spec, n: 100, k: 99}}}
	l.rebuild()
	before := l.score([]int{0, 1})
	deep := l.m.FailureProbability([]int{-24, 0}, spec)
	l.fitEdgeRateShift(&cfg, []int{0, 1})
	if !(l.score([]int{0, 1}) < before) || cfg.Edges[0].Workload["active"] >= -20 {
		t.Fatal("coupled shift did not remove false boundary hazard")
	}
	if got := l.m.FailureProbability([]int{-24, 0}, spec); math.Abs(got-deep) > 1e-12 {
		t.Fatalf("past-edge hazard changed: %g -> %g", deep, got)
	}
	if cfg.Edges[0].Workload["unsupported"] != -50 || cfg.Edges[0].Isolated[0] != -50 {
		t.Fatal("unsupported edges moved")
	}
}
