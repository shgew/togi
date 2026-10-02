package main

import (
	"bytes"
	"strings"
	"testing"

	"github.com/shgew/togi/internal/facts"
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
		{"isolated", facts.TrialFact, []int{1}, []int{0, -30, 0}, -30},
		{"resident", facts.TrialFact, []int{0, 1, 2}, []int{-40, -20, -30}, -20},
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
			check, err := checkModel(tc.path, cfg)
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
			reportModelChecks(&output, []*modelCheck{check})
			if !strings.Contains(output.String(), tc.path+": "+tc.status) {
				t.Fatal(output.String())
			}
		})
	}
}
