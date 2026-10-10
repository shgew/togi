package main

import (
	"bytes"
	"errors"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

const (
	testScenario = "adv"
	testSeeds    = 24
)

func testGate(t *testing.T) *gate {
	t.Helper()
	g := &gate{
		ID:            "test",
		Baseline:      gateBaseline{Ruleset: 10, Commits: []string{"abc"}},
		Split:         "dev",
		Gated:         []string{testScenario},
		Pooled:        []string{testScenario},
		Conclude:      []string{"hand"},
		Quantiles:     []float64{0.5, 0.9},
		Confidence:    0.95,
		Resamples:     10000,
		BootstrapSeed: [2]uint64{1, 1},
		MaxTimeRatio:  2,
	}
	seeds := make([]uint64, testSeeds)
	for i := range seeds {
		seeds[i] = uint64(i + 1)
	}
	if err := g.resolve([]scenario{{Name: testScenario, Dev: seeds}, {Name: "hand", Dev: seeds[:2]}}); err != nil {
		t.Fatal(err)
	}
	return g
}

// gateRuns builds a baseline of 1..24 and a candidate that adds shift(i) to the i-th seed's value.
func gateRuns(shift func(i int) float64, hours func(i int) (candidate, baseline float64)) (candidate, baseline []result) {
	for i := range testSeeds {
		v := float64(i + 1)
		w := v + shift(i)
		ch, bh := 10.0, 10.0
		if hours != nil {
			ch, bh = hours(i)
		}
		candidate = append(candidate, result{Scenario: testScenario, Seed: uint64(i + 1), Split: "dev", Status: "concluded", SimHours: ch, Crashes: 3, WorstR7HazardPerH: &w})
		baseline = append(baseline, result{Scenario: testScenario, Seed: uint64(i + 1), Split: "dev", Status: "concluded", SimHours: bh, WorstR7HazardPerH: &v, Ruleset: 10, Commit: "abc"})
	}
	for seed := uint64(1); seed <= 2; seed++ {
		candidate = append(candidate, result{Scenario: "hand", Seed: seed, Split: "dev", Status: "concluded", SimHours: 1})
		baseline = append(baseline, result{Scenario: "hand", Seed: seed, Split: "dev", Status: "concluded", SimHours: 1, Ruleset: 10, Commit: "abc"})
	}
	return candidate, baseline
}

func none(int) float64 { return 0 }

func find(t *testing.T, r gateResult, kind, scenario string) criterion {
	t.Helper()
	for _, c := range r.Criteria {
		if c.Kind == kind && c.Scenario == scenario {
			return c
		}
	}
	t.Fatalf("no %s criterion for %s in %+v", kind, scenario, r)
	return criterion{}
}

func TestGateQuantileNoise(t *testing.T) {
	g := testGate(t)
	for _, tc := range []struct {
		name  string
		shift func(int) float64
		pass  bool
		above bool
	}{
		{"unchanged", none, true, false},
		{"noise crossing zero", func(i int) float64 { return float64(1 - 2*(i%2)) }, true, false},
		{"every seed worse", func(int) float64 { return 1 }, false, true},
		{"every seed better", func(int) float64 { return -1 }, true, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			candidate, baseline := gateRuns(tc.shift, nil)
			r := judgeGate(g, candidate, baseline)
			for _, kind := range []string{"median", "p90"} {
				c := find(t, r, kind, testScenario)
				if c.Pass != tc.pass {
					t.Errorf("%s pass = %v, want %v (change %+v, interval [%+v, %+v])", kind, c.Pass, tc.pass, c.Change, c.Lo, c.Hi)
				}
				if (c.Lo > 0) != tc.above {
					t.Errorf("%s interval [%+v, %+v], above zero = %v, want %v", kind, c.Lo, c.Hi, c.Lo > 0, tc.above)
				}
			}
		})
	}
}

func TestGateQuantileIsDifferenceOfQuantiles(t *testing.T) {
	g := testGate(t)
	// Every pair's difference is +1 except the largest seed's -23: the median of per-pair
	// differences is +1, but the candidate's median equals the baseline's.
	candidate, baseline := gateRuns(func(i int) float64 {
		if i == testSeeds-1 {
			return -23
		}
		return 1
	}, nil)
	c := find(t, judgeGate(g, candidate, baseline), "median", testScenario)
	if c.Candidate != 12.5 || c.Baseline != 12.5 || c.Change != 0 {
		t.Fatalf("median baseline %v candidate %v change %v, want 12.5, 12.5, 0", c.Baseline, c.Candidate, c.Change)
	}
}

func TestGatePooledMedianStrictlyLower(t *testing.T) {
	g := testGate(t)
	for _, tc := range []struct {
		name  string
		shift float64
		pass  bool
	}{{"equal", 0, false}, {"higher", 1, false}, {"lower", -1, true}} {
		t.Run(tc.name, func(t *testing.T) {
			candidate, baseline := gateRuns(func(int) float64 { return tc.shift }, nil)
			c := find(t, judgeGate(g, candidate, baseline), "pooled_median", testScenario)
			if c.Pass != tc.pass {
				t.Fatalf("pooled median pass = %v, want %v (change %v)", c.Pass, tc.pass, c.Change)
			}
		})
	}
}

func TestGateConclusion(t *testing.T) {
	g := testGate(t)
	for _, tc := range []struct {
		name                string
		scenario            string
		index               int
		candidate, baseline string
		pass                bool
	}{
		{"lost seed", testScenario, 3, "deadend", "concluded", false},
		{"lost hand-set seed", "hand", 25, "censored", "concluded", false},
		{"baseline never concluded", testScenario, 3, "deadend", "deadend", true},
		{"newly concluded", testScenario, 3, "concluded", "deadend", true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			candidate, baseline := gateRuns(none, nil)
			candidate[tc.index].Status, baseline[tc.index].Status = tc.candidate, tc.baseline
			c := find(t, judgeGate(g, candidate, baseline), "conclusion", tc.scenario)
			if c.Pass != tc.pass {
				t.Fatalf("conclusion pass = %v, want %v (lost %v)", c.Pass, tc.pass, c.Lost)
			}
		})
	}
}

func TestGateTimeBound(t *testing.T) {
	g := testGate(t)
	for _, tc := range []struct {
		name      string
		candidate float64
		pass      bool
	}{{"exactly twice", 20, true}, {"above twice", 20.0001, false}, {"faster", 5, true}} {
		t.Run(tc.name, func(t *testing.T) {
			candidate, baseline := gateRuns(none, func(int) (float64, float64) { return tc.candidate, 10 })
			c := find(t, judgeGate(g, candidate, baseline), "time", testScenario)
			if c.Pass != tc.pass {
				t.Fatalf("time pass = %v, want %v (ratio %.12f)", c.Pass, tc.pass, c.Change)
			}
		})
	}
	t.Run("geometric mean of ratios", func(t *testing.T) {
		candidate, baseline := gateRuns(none, func(i int) (float64, float64) { return []float64{40, 10}[i%2], 10 })
		c := find(t, judgeGate(g, candidate, baseline), "time", testScenario)
		if !c.Pass || c.Timed != testSeeds {
			t.Fatalf("time criterion %+v, want pass over every pair (ratios 4 and 1 average to 2)", c)
		}
	})
}

func TestGateRefusals(t *testing.T) {
	g := testGate(t)
	for _, tc := range []struct {
		name   string
		mutate func(candidate, baseline []result) ([]result, []result)
		want   string
	}{
		{"candidate seed missing", func(c, b []result) ([]result, []result) { return c[1:], b }, "incomplete pairing: adv seed 1 has no candidate run"},
		{"baseline seed missing", func(c, b []result) ([]result, []result) { return c, b[1:] }, "incomplete pairing: adv seed 1 has no baseline run"},
		{"hand-set seed missing", func(c, b []result) ([]result, []result) { return c[:len(c)-1], b }, "incomplete pairing: hand seed 2"},
		{"other baseline", func(c, b []result) ([]result, []result) { b[0].Commit = "def"; return c, b }, "not ruleset 10 at abc"},
		{"metric missing", func(c, b []result) ([]result, []result) { c[0].WorstR7HazardPerH = nil; return c, b }, "adv seed 1 lacks worst_r7_hazard_per_h"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			candidate, baseline := tc.mutate(gateRuns(none, nil))
			r := judgeGate(g, candidate, baseline)
			if r.pass() || len(r.Criteria) != 0 || !strings.Contains(r.Refused, tc.want) {
				t.Fatalf("judgeGate = %+v, want refusal containing %q and no criteria", r, tc.want)
			}
		})
	}
}

func TestGateExitsByVerdict(t *testing.T) {
	g := testGate(t)
	for _, tc := range []struct {
		name  string
		shift float64
		drop  int
		want  error
	}{{"pass", -1, 0, nil}, {"fail", 1, 0, errGateFailed}, {"refused", -1, 1, errGateFailed}} {
		t.Run(tc.name, func(t *testing.T) {
			candidate, baseline := gateRuns(func(int) float64 { return tc.shift }, nil)
			var out bytes.Buffer
			if err := judgeAndReport(&out, g, candidate[tc.drop:], baseline); !errors.Is(err, tc.want) {
				t.Fatalf("judgeAndReport = %v, want %v; report:\n%s", err, tc.want, out.String())
			}
			if want := "gate test: " + map[string]string{"pass": "PASS", "fail": "FAIL", "refused": "FAIL"}[tc.name]; !strings.Contains(out.String(), want) {
				t.Errorf("report lacks %q:\n%s", want, out.String())
			}
		})
	}
}

func TestGateBaselineIdentity(t *testing.T) {
	g := testGate(t)
	candidate, baseline := gateRuns(none, nil)
	baseline[0].Commit = "def"
	g.Baseline.Commits = []string{"abc", "def"}
	if r := judgeGate(g, candidate, baseline); r.Refused != "" {
		t.Fatalf("a baseline mixing the named commits was refused: %s", r.Refused)
	}
	baseline[1].Ruleset = 9
	if r := judgeGate(g, candidate, baseline); !strings.Contains(r.Refused, "is ruleset 9 at abc, not ruleset 10 at abc or def") {
		t.Fatalf("refused = %q", r.Refused)
	}
}

func TestCommittedBaselineIsTheGatesBaseline(t *testing.T) {
	g, err := loadGate("suite.toml")
	if err != nil || g == nil {
		t.Fatalf("loadGate = %v, %v", g, err)
	}
	baseline, err := readResults("baseline.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	for _, r := range baseline {
		if r.Ruleset != g.Baseline.Ruleset || !slices.Contains(g.Baseline.Commits, r.Commit) {
			t.Fatalf("baseline run %s/%d is ruleset %d at %s; the gate names ruleset %d at %v: a pull request that re-records baseline runs lists its commit in [gate.baseline]", r.Scenario, r.Seed, r.Ruleset, r.Commit, g.Baseline.Ruleset, g.Baseline.Commits)
		}
	}
	have := make(map[key]bool, len(baseline))
	for _, r := range baseline {
		have[key{r.Scenario, r.Seed}] = true
	}
	for name, seeds := range g.seeds {
		for _, seed := range seeds {
			if !have[key{name, seed}] {
				t.Errorf("baseline.jsonl has no run for the gate's %s seed %d", name, seed)
			}
		}
	}
}

func TestGateBootstrapReproducible(t *testing.T) {
	candidate, baseline := gateRuns(func(i int) float64 { return float64(i%5) - 1 }, nil)
	var a, b []float64
	for _, r := range candidate[:testSeeds] {
		a = append(a, *r.WorstR7HazardPerH)
	}
	for _, r := range baseline[:testSeeds] {
		b = append(b, *r.WorstR7HazardPerH)
	}
	_, _, lo, hi := quantileChange(a, b, 0.5, 0.95, 10000, [2]uint64{1, 1})
	for range 3 {
		_, _, lo2, hi2 := quantileChange(a, b, 0.5, 0.95, 10000, [2]uint64{1, 1})
		if lo != lo2 || hi != hi2 {
			t.Fatalf("interval [%v, %v] then [%v, %v]", lo, hi, lo2, hi2)
		}
	}
	if lo != -0.5 || hi != 2.5 {
		t.Errorf("interval = [%v, %v], want the pinned [-0.5, 2.5]", lo, hi)
	}
}

func TestGateReportGolden(t *testing.T) {
	g := testGate(t)
	var got bytes.Buffer
	for _, tc := range []struct {
		name  string
		shift func(int) float64
		drop  int
	}{
		{"pass", func(int) float64 { return -1 }, 0},
		{"fail", func(i int) float64 { return float64(i%2) + 1 }, 0},
		{"refused", none, 1},
	} {
		candidate, baseline := gateRuns(tc.shift, nil)
		candidate[5].Status = map[bool]string{true: "deadend", false: "concluded"}[tc.name == "fail"]
		reportGate(&got, g, judgeGate(g, candidate[tc.drop:], baseline))
		got.WriteString("\n")
	}
	path := filepath.Join("testdata", "gate.golden")
	if *update {
		if err := os.WriteFile(path, got.Bytes(), 0600); err != nil {
			t.Fatal(err)
		}
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(string(want), got.String()); diff != "" {
		t.Fatal(diff)
	}
}

func TestLoadGate(t *testing.T) {
	t.Run("committed suite", func(t *testing.T) {
		g, err := loadGate("suite.toml")
		if err != nil {
			t.Fatal(err)
		}
		if g != nil && !slices.Contains(g.Conclude, "target-nonmember-mce") {
			t.Fatalf("synthetic conclusion guard missing: %+v", g)
		}
		if g == nil || g.ID != "ruleset-11" || g.Baseline.Ruleset != 10 || g.MaxTimeRatio != 2 || g.Resamples != 10000 || g.BootstrapSeed != [2]uint64{1, 1} {
			t.Fatalf("gate = %+v", g)
		}
		for _, name := range append(append([]string(nil), g.Gated...), g.Conclude...) {
			if len(g.seeds[name]) == 0 {
				t.Errorf("no seeds resolved for %s", name)
			}
		}
		if len(g.seeds["target-r7-vf-boost"]) != 24 {
			t.Errorf("target-r7-vf-boost seeds = %d, want 24", len(g.seeds["target-r7-vf-boost"]))
		}
	})
	if err := (&gate{ID: "g", Baseline: gateBaseline{1, []string{"x"}}, Split: "all", Gated: []string{"a"}, Pooled: []string{"b"}, Quantiles: []float64{0.5}, Confidence: 0.95, Resamples: 10, MaxTimeRatio: 2}).resolve([]scenario{{Name: "a", Dev: []uint64{1}}, {Name: "b", Dev: []uint64{1}}}); err == nil || !strings.Contains(err.Error(), `pooled scenario "b" is not gated`) {
		t.Fatalf("resolve error = %v", err)
	}
	scenarios := "\n[[scenario]]\nname = 'a'\ndev = [1]\nholdout = [101]\n"
	valid := "[gate]\nid = 'g'\nsplit = 'all'\ngated = ['a']\nquantiles = [0.5]\nconfidence = 0.95\nresamples = 10\nmax_time_ratio = 2\n[gate.baseline]\nruleset = 1\ncommits = ['x']\n"
	for _, tc := range []struct {
		name, suite, want string
	}{
		{"valid", valid + scenarios, ""},
		{"no gate", scenarios, ""},
		{"unknown gate key", strings.Replace(valid, "id = 'g'", "id = 'g'\nextra = 1", 1) + scenarios, "unknown suite key gate.extra"},
		{"unknown scenario", strings.Replace(valid, "['a']", "['b']", 1) + scenarios, `unknown scenario "b"`},
		{"bad quantile", strings.Replace(valid, "[0.5]", "[1]", 1) + scenarios, "quantile 1 outside"},
		{"bad split", strings.Replace(valid, "'all'", "'x'", 1) + scenarios, "split must be"},
		{"no baseline", strings.Replace(valid, "commits = ['x']", "", 1) + scenarios, "needs an id and a baseline"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "suite.toml")
			if err := os.WriteFile(path, []byte(tc.suite), 0600); err != nil {
				t.Fatal(err)
			}
			_, err := loadGate(path)
			if tc.want == "" {
				if err != nil {
					t.Fatal(err)
				}
				return
			}
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("loadGate error = %v, want %q", err, tc.want)
			}
		})
	}
}
