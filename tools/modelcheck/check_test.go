package modelcheck

import (
	"bytes"
	"math"
	"strings"
	"testing"

	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/tools/trialfacts"
)

func TestBinomialInterval(t *testing.T) {
	for _, tc := range []struct {
		n    int
		p    float64
		want [2]int
	}{
		{10, 0, [2]int{0, 0}}, {10, 1e-12, [2]int{0, 0}},
		{10, 1, [2]int{10, 10}}, {10, 1 - 1e-12, [2]int{10, 10}},
		{10, 0.5, [2]int{1, 9}}, {10, 0.1, [2]int{0, 4}},
		{10000, 0.5, [2]int{4871, 5129}},
	} {
		if got := binomialInterval(tc.n, tc.p); got != tc.want {
			t.Errorf("n=%d p=%g: got %v want %v", tc.n, tc.p, got, tc.want)
		}
	}
}

func TestModelGrouping(t *testing.T) {
	for _, tc := range []struct {
		name           string
		kind           facts.Kind
		cores, profile []int
		depth          int
	}{
		{"alone", facts.TrialFact, []int{1}, []int{0, -30, 0}, -30},
		{"together", facts.TrialFact, []int{0, 1, 2}, []int{-40, -20, -30}, -20},
		{"any-zero", facts.TrialFact, []int{0, 1, 2}, []int{-40, 0, -30}, 0},
		{"idle", facts.IdleFact, []int{0, 1, 2}, []int{-40, -20, -30}, -20},
	} {
		t.Run(tc.name, func(t *testing.T) {
			r := trialfacts.Record{Kind: tc.kind, Class: facts.Class{Regime: machine.R6, Cores: tc.cores}, Profile: tc.profile}
			g := groupOf(r)
			if g.Depth != tc.depth || g.Kind != tc.kind {
				t.Fatalf("got %+v", g)
			}
		})
	}
}

func TestFixtureModelCheck(t *testing.T) {
	for _, tc := range []struct {
		path, status string
		interval     [2]int
	}{
		{"testdata/model-ok.toml", "ok", [2]int{0, 0}},
		{"testdata/model-flagged.toml", "flagged", [2]int{10, 10}},
	} {
		t.Run(tc.status, func(t *testing.T) {
			cfg, err := sim.LoadMachine(tc.path)
			if err != nil {
				t.Fatal(err)
			}
			check, err := Check(tc.path, cfg, trialfacts.Extracts{})
			if err != nil {
				t.Fatal(err)
			}
			if check.Status != tc.status || len(check.Groups) != 1 {
				t.Fatalf("got %+v", check)
			}
			g := check.Groups[0]
			if g.N != 10 || g.K != 0 || g.Interval != tc.interval {
				t.Fatalf("got %+v", g)
			}
			var output bytes.Buffer
			Report(&output, []*Result{check})
			if !strings.Contains(output.String(), tc.path+": "+tc.status) {
				t.Fatal(output.String())
			}
		})
	}
}

func TestCheckerConstraintsPreserveBinomialCheck(t *testing.T) {
	cfg := sim.Config{Cores: 2, Limits: []sim.Limits{
		{Alone: [5]int{-50, -50, -50, -50, -50}, Together: [7]int{-50, -50, -50, -50, -50, -50, -50}},
		{Alone: [5]int{-50, -50, -50, -50, -50}, Together: [7]int{-50, -50, -50, -50, -50, -50, -50}},
	}}
	var records []trialfacts.Record
	for i := range 20 {
		r := trialfacts.Record{Kind: facts.TrialFact, Profile: []int{-10, 0}, Class: facts.Class{Regime: machine.R1, Cores: []int{0}, DurationS: 90}, Outcome: journal.OutcomePass}
		if i < 2 {
			r.Outcome = journal.OutcomeFailure
		}
		records = append(records, r)
	}
	checker, err := NewChecker(cfg, records)
	if err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct {
		p    float64
		want bool
	}{{0, false}, {0.001, false}, {0.1, true}, {0.9, false}} {
		cfg.Limits[0].Flat = -math.Log1p(-tc.p) / 90
		m, err := sim.New(cfg)
		if err != nil {
			t.Fatal(err)
		}
		result := checker.Check("synthetic", "synthetic", m)
		if checker.Accepts(m) != tc.want || (result.Status == "ok") != tc.want {
			t.Errorf("p=%g: acceptance=%v report=%+v want %v", tc.p, checker.Accepts(m), result, tc.want)
		}
	}
}

func TestReportOmitsAbsentChecks(t *testing.T) {
	var output bytes.Buffer
	Report(&output, nil)
	if output.Len() != 0 {
		t.Fatalf("absent checks printed a model diagnostic: %q", output.String())
	}
}
