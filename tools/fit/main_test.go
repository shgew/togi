package main

import (
	"bytes"
	"compress/gzip"
	"encoding/json"
	"flag"
	"fmt"
	"math"
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
	cfg.CCD = &sim.CCD{LogRate: -9, Slope: 0.1, Effect: [2]float64{0.2, -0.3}}
	idle := -30
	cfg.Facts = "../facts/extract.jsonl.gz"
	cfg.Limits[0].Idle = &idle
	cfg.Limits[0].Workload = map[string]int{"z": -24, "a": -20}
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
	if diff := cmp.Diff(cfg.Limits, got.Limits); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff(cfg.BIOSContext, got.BIOSContext); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff(cfg.CCD, got.CCD); diff != "" {
		t.Fatal(diff)
	}
	if got.Facts != cfg.Facts || got.Model.PastLimitRate != cfg.Model.PastLimitRate || got.Model.Growth != cfg.Model.Growth || got.Model.NearLimitRate != cfg.Model.NearLimitRate {
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
		for i := range 12 {
			outcome := journal.OutcomePass
			if i >= 6 {
				outcome = journal.OutcomeFailure
			}
			r := trialfacts.Record{Kind: facts.TrialFact, Outcome: outcome, Profile: []int{-10, 0}, Context: &context, Class: facts.Class{Regime: machine.R1, Cores: []int{0}, DurationS: 60}}
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
		for jobs, dir := range []string{"first", "second"} {
			out := filepath.Join(root, dir)
			var report bytes.Buffer
			if err := generate(extract, out, 263, 1, jobs+1, &report); err != nil {
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
				check, err := modelcheck.Check(path, cfg, trialfacts.Extracts{})
				if err != nil {
					t.Fatal(err)
				}
				if check.Status != "ok" || len(check.Groups) != 1 || check.Groups[0].N != 12 || check.Groups[0].K != 6 {
					t.Fatalf("fit lost original evidence: %+v", check)
				}
				// Seed 264 draws indices 7,11,11,8,8,10,4,5,7,2,1,0: seven failures.
				failures := 6 + n
				prediction := check.Groups[0].MeanP
				if math.Abs(prediction-float64(failures)/12) > 0.02 {
					t.Fatalf("member %d resampled prediction = %g, want %d/12", n, prediction, failures)
				}
				if dir == "first" {
					first = append(first, content)
				} else if diff := cmp.Diff(first[n], content); diff != "" {
					t.Fatalf("nonreproducible member %d: %s", n, diff)
				}
				found := false
				for line := range strings.SplitSeq(report.String(), "\n") {
					evidence, ok := strings.CutPrefix(line, path+": ")
					if !ok {
						continue
					}
					var trials int
					var loss float64
					if fields, err := fmt.Sscanf(evidence, "%d trials; negative log likelihood %f", &trials, &loss); err != nil || fields != 2 {
						continue
					}
					wantLoss := -float64(failures)*math.Log(prediction) - float64(12-failures)*math.Log1p(-prediction)
					if trials != 12 || math.Abs(loss-wantLoss) > 0.0001 {
						t.Fatalf("member %d resampled likelihood = %d trials, %g; want 12 trials, %g", n, trials, loss, wantLoss)
					}
					found = true
				}
				if !found || !strings.Contains(report.String(), path+": ok (1 eligible groups; 0 idle failures without trial exposure)") {
					t.Fatalf("missing fit evidence: %s", report.String())
				}
			}
			if !strings.Contains(report.String(), "Fit elapsed: 0s") {
				t.Fatalf("elapsed output: %s", report.String())
			}
		}
	})
}

func TestGenerateConstrainedRefitsMatchSerial(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		root := t.TempDir()
		extract := filepath.Join(root, "facts.jsonl.gz")
		var compressed bytes.Buffer
		gz := gzip.NewWriter(&compressed)
		encoder := json.NewEncoder(gz)
		context := machine.BIOSContext{Board: "fixture", BIOSVersion: "A", CPUModel: "Zen 5 fixture", Microcode: "0x1", BoostLimitMHz: 5600}
		for i := range 30 {
			outcome := journal.OutcomePass
			if i == 0 {
				outcome = journal.OutcomeFailure
			}
			r := trialfacts.Record{Kind: facts.TrialFact, Outcome: outcome, Profile: []int{-10, 0}, Context: &context, Class: facts.Class{Regime: machine.R1, Cores: []int{0}, DurationS: 60}}
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
		out := filepath.Join(root, "machines")
		generated := func(jobs int) (string, [][]byte, []float64) {
			var report bytes.Buffer
			if err := generate(extract, out, 263, 4, jobs, &report); err != nil {
				t.Fatal(err)
			}
			var machines [][]byte
			var predictions []float64
			for n := range 5 {
				path := filepath.Join(out, "target-fit-"+strconv.Itoa(n)+".toml")
				content, err := os.ReadFile(path)
				if err != nil {
					t.Fatal(err)
				}
				machines = append(machines, content)
				cfg, err := sim.LoadMachine(path)
				if err != nil {
					t.Fatal(err)
				}
				check, err := modelcheck.Check(path, cfg, trialfacts.Extracts{})
				if err != nil {
					t.Fatal(err)
				}
				if check.Status != "ok" || len(check.Groups) != 1 {
					t.Fatalf("jobs %d member %d fails the original evidence: %+v", jobs, n, check)
				}
				predictions = append(predictions, check.Groups[0].MeanP)
			}
			return report.String(), machines, predictions
		}
		serialReport, serialMachines, predictions := generated(1)
		// Seeds 264-266 resample none of the single failure, so refits 1-3 are flagged and refit from fit 0.
		for n := 1; n <= 3; n++ {
			if !strings.Contains(serialReport, "Refit "+strconv.Itoa(n)+" constraint: R1  cores=[0] duration=60s depth=-10 n=30 k=1 ") {
				t.Fatalf("refit %d not constrained:\n%s", n, serialReport)
			}
			if predictions[n] >= predictions[0] {
				t.Fatalf("refit %d kept the all-facts prediction %g, want below it: %g", n, predictions[0], predictions[n])
			}
			path := filepath.Join(out, "target-fit-"+strconv.Itoa(n)+".toml")
			wantLoss := -30 * math.Log1p(-predictions[n])
			var loss float64
			found := false
			for line := range strings.SplitSeq(serialReport, "\n") {
				evidence, ok := strings.CutPrefix(line, path+": ")
				if !ok {
					continue
				}
				if fields, err := fmt.Sscanf(evidence, "30 trials; negative log likelihood %f", &loss); err == nil && fields == 1 {
					found = true
				}
			}
			if !found || math.Abs(loss-wantLoss) > 0.0001 {
				t.Fatalf("refit %d reported likelihood %g (found %t); want %g on its failure-free resample:\n%s", n, loss, found, wantLoss, serialReport)
			}
		}
		if strings.Contains(serialReport, "Refit 4 constraint") {
			t.Fatalf("refit 4 constrained:\n%s", serialReport)
		}
		parallelReport, parallelMachines, _ := generated(5)
		if diff := cmp.Diff(serialReport, parallelReport); diff != "" {
			t.Fatalf("parallel report differs from serial (-serial +parallel):\n%s", diff)
		}
		if diff := cmp.Diff(serialMachines, parallelMachines); diff != "" {
			t.Fatalf("parallel machines differ from serial (-serial +parallel):\n%s", diff)
		}
	})
}

func TestGenerateRefusesMissingOrNondecisiveEvidence(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		root := t.TempDir()
		extract := filepath.Join(root, "facts.jsonl.gz")
		out := filepath.Join(root, "machines")
		var output bytes.Buffer
		if err := generate(extract, out, 263, 0, 2, &output); err == nil {
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
		if err := generate(extract, out, 263, 0, 2, &output); err == nil || err.Error() != "extract has no decisive trials" {
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

func TestGenerateRefusesConflictingDestinations(t *testing.T) {
	for _, directory := range []bool{false, true} {
		t.Run(map[bool]string{false: "output directory", true: "fitted file"}[directory], func(t *testing.T) {
			root := t.TempDir()
			extract := filepath.Join(root, "facts.jsonl.gz")
			var compressed bytes.Buffer
			gz := gzip.NewWriter(&compressed)
			record := trialfacts.Record{Kind: facts.TrialFact, Outcome: journal.OutcomePass, Profile: []int{-10, 0}, Class: facts.Class{Regime: machine.R1, Cores: []int{0}, DurationS: 60}}
			if err := json.NewEncoder(gz).Encode(record); err != nil {
				t.Fatal(err)
			}
			if err := gz.Close(); err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(extract, compressed.Bytes(), 0600); err != nil {
				t.Fatal(err)
			}
			out := filepath.Join(root, "machines")
			conflict := out
			want := "create output directory"
			if directory {
				conflict = filepath.Join(out, "target-fit-0.toml")
				want = "fit 0 write fitted machine"
				if err := os.MkdirAll(conflict, 0700); err != nil {
					t.Fatal(err)
				}
			} else if err := os.WriteFile(conflict, []byte("preserve"), 0600); err != nil {
				t.Fatal(err)
			}
			var report bytes.Buffer
			if err := generate(extract, out, 263, 0, 2, &report); err == nil || !strings.HasPrefix(err.Error(), want+":") || report.Len() != 0 {
				t.Fatalf("conflicting destination: %v, report %q; want %s", err, report.String(), want)
			}
			info, err := os.Stat(conflict)
			if err != nil || info.IsDir() != directory {
				t.Fatalf("conflicting destination replaced: %v, %v", info, err)
			}
			if !directory {
				data, err := os.ReadFile(conflict)
				if err != nil || string(data) != "preserve" {
					t.Fatalf("conflicting file changed: %q, %v", data, err)
				}
			}
		})
	}
}

func TestRunForwardOnly(t *testing.T) {
	root := t.TempDir()
	extract := filepath.Join(root, "facts.jsonl.gz")
	var compressed bytes.Buffer
	gz := gzip.NewWriter(&compressed)
	encoder := json.NewEncoder(gz)
	for i, session := range []string{"20260101T000000Z", "20260102T000000Z"} {
		record := trialfacts.Record{Session: session, Seq: 1, Kind: facts.TrialFact, Outcome: journal.OutcomePass, Profile: []int{-10, 0}, Class: facts.Class{Regime: machine.R1, Cores: []int{0}, DurationS: 60}}
		if i == 1 {
			record.Outcome = journal.OutcomeFailure
		}
		if err := encoder.Encode(record); err != nil {
			t.Fatal(err)
		}
	}
	if err := gz.Close(); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(extract, compressed.Bytes(), 0o600); err != nil {
		t.Fatal(err)
	}
	out := filepath.Join(root, "machines")
	for _, tc := range []struct {
		args   []string
		code   int
		output string
	}{
		{[]string{"--forward-only"}, 0, "20260102T000000Z ruleset=0 training_sessions=1: trials=1 failures=1"},
		{[]string{"--forward-only", "--seal", "1"}, 1, "fit: --seal 1 leaves no held-out session to score (1 held out)"},
		{[]string{"--seal", "1"}, 2, "only with --forward-only"},
		{[]string{"--forward-only", "--seal", "-1"}, 2, "nonnegative --seal"},
		{[]string{"--forward-only", "--jobs", "0"}, 2, "positive --jobs"},
		{[]string{"--forward-only", "--jobs", "-1"}, 2, "positive --jobs"},
	} {
		var stdout, stderr bytes.Buffer
		code := run(append([]string{"--facts", extract, "--out", out}, tc.args...), &stdout, &stderr)
		if code != tc.code || !strings.Contains(stdout.String()+stderr.String(), tc.output) {
			t.Errorf("fit %v = %d, stdout %q, stderr %q; want %d and %q", tc.args, code, stdout.String(), stderr.String(), tc.code, tc.output)
		}
	}
	if _, err := os.Stat(out); !os.IsNotExist(err) {
		t.Fatalf("forward-only run created the output directory: %v", err)
	}
}
