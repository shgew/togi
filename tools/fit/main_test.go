package main

import (
	"bytes"
	"compress/gzip"
	"encoding/json"
	"flag"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"testing/synctest"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/tools/modelcheck"
	"github.com/shgew/togi/tools/trialfacts"
)

var update = flag.Bool("update", false, "rewrite testdata/*.golden from the current output")

func TestEncodeMachineRetainsFitEvidenceAndParameters(t *testing.T) {
	cfg := initialConfig([]trialfacts.Record{{Profile: []int{0, 0}, Context: &machine.BIOSContext{Board: "fixture", BIOSVersion: "A", CPUModel: "Zen 5 fixture", Microcode: "0x1", BoostLimitMHz: 5600}}})
	idle := -30
	cfg.Facts = "../facts/extract.jsonl.gz"
	cfg.Edges[0].Idle = &idle
	cfg.Edges[0].Workload = map[string]int{"z": -24, "a": -20}
	groups := []modelcheck.Group{{Class: facts.Class{Regime: machine.R7, Workload: "work", Cores: []int{0, 1}, DurationS: 120}, Depth: -24, N: 45, K: 5}}
	content := encodeMachine(cfg, 2, 263, 45, 12.5, groups)
	path := filepath.Join(t.TempDir(), "fit.toml")
	if err := os.WriteFile(path, content, 0o600); err != nil {
		t.Fatal(err)
	}
	got, err := sim.LoadMachine(path)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(cfg.Edges, got.Edges); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff(cfg.BIOSContext, got.BIOSContext); diff != "" {
		t.Fatal(diff)
	}
	if got.Facts != cfg.Facts || got.Model.PastEdgeRate != cfg.Model.PastEdgeRate || got.Model.Growth != cfg.Model.Growth || got.Model.NearEdgeRate != cfg.Model.NearEdgeRate {
		t.Fatalf("encoded fit lost parameters: %+v", got)
	}
	if *update {
		if err := os.MkdirAll("testdata", 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile("testdata/encoded-fit.golden", content, 0o644); err != nil {
			t.Fatal(err)
		}
	}
	golden, err := os.ReadFile("testdata/encoded-fit.golden")
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(string(golden), string(content)); diff != "" {
		t.Fatal(diff)
	}
}

func TestGenerateWritesCheckedReproducibleEnsemble(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		root := t.TempDir()
		extract := filepath.Join(root, "facts.jsonl.gz")
		var compressed bytes.Buffer
		gz := gzip.NewWriter(&compressed)
		encoder := json.NewEncoder(gz)
		context := machine.BIOSContext{Board: "fixture", BIOSVersion: "A", CPUModel: "Zen 5 fixture", Microcode: "0x1", BoostLimitMHz: 5600}
		for range 10 {
			r := trialfacts.Record{Kind: facts.TrialFact, Outcome: journal.OutcomePass, Profile: []int{-10, 0}, Context: &context, Class: facts.Class{Regime: machine.R1, Cores: []int{0}, DurationS: 60}}
			if err := encoder.Encode(r); err != nil {
				t.Fatal(err)
			}
		}
		if err := gz.Close(); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(extract, compressed.Bytes(), 0o600); err != nil {
			t.Fatal(err)
		}
		var first [][]byte
		for _, dir := range []string{"first", "second"} {
			out := filepath.Join(root, dir)
			var report bytes.Buffer
			if err := generate(extract, out, 263, 1, &report); err != nil {
				t.Fatal(err)
			}
			for n := range 2 {
				path := filepath.Join(out, "target-fit-"+strconv.Itoa(n)+".toml")
				content, err := os.ReadFile(path)
				if err != nil {
					t.Fatal(err)
				}
				cfg, err := sim.LoadMachine(path)
				if err != nil {
					t.Fatal(err)
				}
				if cfg.Facts != "../facts.jsonl.gz" {
					t.Fatalf("extract provenance: %q", cfg.Facts)
				}
				if diff := cmp.Diff(context, cfg.BIOSContext); diff != "" {
					t.Fatal(diff)
				}
				check, err := modelcheck.Check(path, cfg)
				if err != nil {
					t.Fatal(err)
				}
				if check.Status != "ok" || len(check.Groups) != 1 || check.Groups[0].N != 10 || check.Groups[0].K != 0 {
					t.Fatalf("fit lost original evidence: %+v", check)
				}
				if dir == "first" {
					first = append(first, content)
				} else if diff := cmp.Diff(first[n], content); diff != "" {
					t.Fatalf("nonreproducible member %d: %s", n, diff)
				}
				if !strings.Contains(report.String(), path+": 10 starts; negative log likelihood 0.000000") || !strings.Contains(report.String(), path+": ok (1 eligible groups; 0 idle failures without start exposure)") {
					t.Fatalf("missing fit evidence: %s", report.String())
				}
			}
			if !strings.Contains(report.String(), "Fit elapsed: 0s") {
				t.Fatalf("elapsed output: %s", report.String())
			}
		}
	})
}

func TestGenerateRefusesMissingOrNondecisiveEvidence(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		root := t.TempDir()
		extract := filepath.Join(root, "facts.jsonl.gz")
		out := filepath.Join(root, "machines")
		var output bytes.Buffer
		if err := generate(extract, out, 263, 0, &output); err == nil {
			t.Fatal("missing extract accepted")
		}
		var compressed bytes.Buffer
		gz := gzip.NewWriter(&compressed)
		if err := gz.Close(); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(extract, compressed.Bytes(), 0o600); err != nil {
			t.Fatal(err)
		}
		if err := generate(extract, out, 263, 0, &output); err == nil || err.Error() != "extract has no decisive starts" {
			t.Fatalf("empty extract: %v", err)
		}
		if _, err := os.Stat(out); !os.IsNotExist(err) {
			t.Fatalf("refused evidence created output: %v", err)
		}
		if output.Len() != 0 {
			t.Fatalf("refused evidence reported a fit: %q", output.String())
		}
	})
}
