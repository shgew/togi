package main

import (
	"bytes"
	"flag"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
)

var update = flag.Bool("update", false, "update golden files")

func TestBenchReports(t *testing.T) {
	candidate := []result{
		{Scenario: "zeta", Seed: 2, Split: "dev", Status: "concluded", SimHours: 4, Crashes: 2, Depth: -20, HazardMaxPerH: 0.005, Trials: 2, RealAnswers: 1, CleanRotations: 2, PartialSeconds: 180},
		{Scenario: "alpha", Seed: 1, Split: "dev", Status: "concluded", SimHours: 2, Depth: -40, Trials: 6, RealAnswers: 1, CleanRotations: 1, PartialSeconds: 120},
		{Scenario: "zeta", Seed: 3, Split: "dev", Status: "timeout", SimHours: 10, Crashes: 4, Depth: -10, HazardMaxPerH: 0.015, Trials: 8, RealAnswers: 1, CleanRotations: 1, PartialSeconds: 30},
	}
	baseline := []result{
		{Scenario: "alpha", Seed: 1, Split: "dev", Status: "concluded", SimHours: 4, Depth: -40, Trials: 4, RealAnswers: 2, CleanRotations: 2, PartialSeconds: 10},
		{Scenario: "zeta", Seed: 2, Split: "dev", Status: "concluded", SimHours: 8, Crashes: 1, Depth: -20, HazardMaxPerH: 0.005, Trials: 4, RealAnswers: 3, CleanRotations: 1, PartialSeconds: 20},
		{Scenario: "alpha", Seed: 9, Split: "holdout", Status: "concluded", SimHours: 1},
	}
	for _, tc := range []struct {
		name   string
		render func(*bytes.Buffer)
	}{
		{"summary", func(w *bytes.Buffer) { reportSummary(w, candidate) }},
		{"comparison", func(w *bytes.Buffer) { reportComparison(w, candidate[:2], baseline) }},
		{"comparison-unmatched", func(w *bytes.Buffer) {
			base := append([]result(nil), baseline...)
			base = append(base, result{Scenario: "missing", Seed: 7, Split: "dev"})
			reportComparison(w, candidate, base)
		}},
		{"comparison-rejected", func(w *bytes.Buffer) {
			a := candidate[0]
			a.Status = "deadend"
			a.Depth = -14
			a.HazardMaxPerH = 0.1
			reportComparison(w, []result{a}, baseline[1:2])
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var got bytes.Buffer
			tc.render(&got)
			path := filepath.Join("testdata", tc.name+".golden")
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
		})
	}
}
