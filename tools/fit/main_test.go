package main

import (
	"bytes"
	"compress/gzip"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"math"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"testing/synctest"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"
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
	fitSignals(&cfg, []trialfacts.Record{
		{Class: facts.Class{Regime: machine.R7}, Outcome: journal.OutcomeFailure, Signal: machine.Crash},
		{Class: facts.Class{Regime: machine.R7}, Outcome: journal.OutcomeFailure, Signal: machine.UncorrectedMCE},
		{Class: facts.Class{Regime: machine.R7}, Outcome: journal.OutcomeFailure, Signal: machine.ComputationError},
		{Class: facts.Class{Regime: machine.R2}, Outcome: journal.OutcomeFailure, Signal: machine.ComputationError},
		{Class: facts.Class{Regime: machine.R2}, Outcome: journal.OutcomeFailure},
		{Class: facts.Class{Regime: machine.R1}, Outcome: journal.OutcomePass},
	})
	groups := []modelcheck.Group{{Class: facts.Class{Regime: machine.R7, Workload: "work", Cores: []int{0, 1}, DurationS: 120}, Depth: -24, N: 45, K: 5}}
	flagged := []modelcheck.Group{{Class: facts.Class{Regime: machine.R7, Workload: "work", Cores: []int{0, 1}, DurationS: 300}, Depth: -24, N: 24, K: 7, Interval: [2]int{0, 2}, Flagged: true}}
	content := encodedMachine(t, cfg, 2, 263, 45, 12.5, groups, flagged)
	path := filepath.Join(t.TempDir(), "fit.json")
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
	wantSignals := map[machine.Signal]float64{machine.Crash: 2, machine.ComputationError: 2}
	wantRegimes := map[machine.Regime]map[machine.Signal]float64{machine.R7: {machine.Crash: 2, machine.ComputationError: 1}, machine.R2: {machine.ComputationError: 1}}
	if diff := cmp.Diff([]any{wantSignals, wantRegimes}, []any{got.Model.Signals, got.Model.RegimeSignals}); diff != "" {
		t.Fatalf("encoded signal mix (-want +got):\n%s", diff)
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
	type evidence struct {
		Trials int
		Loss   float64
	}
	var first [][]byte
	for jobs, dir := range []string{"first", "second"} {
		out := filepath.Join(root, dir)
		fits, err := generate(extract, out, 263, 1, jobs+1, io.Discard)
		if err != nil {
			t.Fatal(err)
		}
		for n := range 2 {
			path := filepath.Join(out, "target-fit-"+strconv.Itoa(n)+".json")
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
			if check.Status != "ok" || len(check.Groups) != 1 || check.Groups[0].N != 12 || check.Groups[0].K != 6 || check.IdleFailures != 0 {
				t.Fatalf("fit lost original evidence: %+v", check)
			}
			if diff := cmp.Diff(check, fits[n].check); diff != "" {
				t.Fatalf("member %d reported check differs from the written file's (-file +reported):\n%s", n, diff)
			}
			// Seed 264 draws indices 7,11,11,8,8,10,4,5,7,2,1,0: seven failures.
			failures := 6 + n
			prediction := check.Groups[0].MeanP
			if math.Abs(prediction-float64(failures)/12) > 0.02 {
				t.Fatalf("member %d resampled prediction = %g, want %d/12", n, prediction, failures)
			}
			want := evidence{12, -float64(failures)*math.Log(prediction) - float64(12-failures)*math.Log1p(-prediction)}
			if diff := cmp.Diff(want, evidence{len(fits[n].sample), fits[n].loss}, cmpopts.EquateApprox(0, 0.0001)); diff != "" {
				t.Fatalf("member %d resampled likelihood (-want +got):\n%s", n, diff)
			}
			if dir == "first" {
				first = append(first, content)
			} else if diff := cmp.Diff(first[n], content); diff != "" {
				t.Fatalf("nonreproducible member %d: %s", n, diff)
			}
		}
	}
}

func TestGenerateFlaggedMembersMatchSerial(t *testing.T) {
	context := machine.BIOSContext{Board: "fixture", BIOSVersion: "A", CPUModel: "Zen 5 fixture", Microcode: "0x1", BoostLimitMHz: 5600}
	class := facts.Class{Regime: machine.R1, Cores: []int{0}, DurationS: 60}
	trials := func(depth, n, failures int) []trialfacts.Record {
		var records []trialfacts.Record
		for i := range n {
			outcome, signal := journal.OutcomePass, machine.Signal("")
			if i < failures {
				outcome, signal = journal.OutcomeFailure, machine.ComputationError
			}
			records = append(records, trialfacts.Record{Kind: facts.TrialFact, Outcome: outcome, Signal: signal, Profile: []int{depth, 0}, Context: &context, Class: class})
		}
		return records
	}
	group := func(depth, n, k int) modelcheck.Group {
		return modelcheck.Group{Context: &context, Kind: facts.TrialFact, Class: class, Depth: depth, N: n, K: k, Flagged: true}
	}
	tests := []struct {
		name        string
		records     []trialfacts.Record
		constrained [][]modelcheck.Group
		flagged     [][]modelcheck.Group
		unseeded    []int
	}{
		{
			name:    "all-facts fit passes",
			records: trials(-10, 30, 1),
			// Seeds 264-266 resample none of the single failure, so refits 1-3 are flagged and refit from fit 0.
			constrained: [][]modelcheck.Group{nil, {group(-10, 30, 1)}, {group(-10, 30, 1)}, {group(-10, 30, 1)}, nil},
			flagged:     make([][]modelcheck.Group, 5),
		},
		{
			name: "all-facts fit fails",
			// Failing at -5 but never at the deeper -10 fits no hazard that grows with depth, so every member flags -5.
			records:     append(trials(-10, 30, 0), trials(-5, 30, 15)...),
			constrained: make([][]modelcheck.Group, 5),
			flagged:     [][]modelcheck.Group{{group(-5, 30, 15)}, {group(-5, 30, 15)}, {group(-5, 30, 15)}, {group(-5, 30, 15)}, {group(-5, 30, 15)}},
			unseeded:    []int{1, 2, 3, 4},
		},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				root := t.TempDir()
				extract := filepath.Join(root, "facts.jsonl.gz")
				var compressed bytes.Buffer
				gz := gzip.NewWriter(&compressed)
				encoder := json.NewEncoder(gz)
				for _, r := range test.records {
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
				generated := func(jobs int) (string, [][]byte, []fitted) {
					var report bytes.Buffer
					fits, err := generate(extract, out, 263, 4, jobs, &report)
					if err != nil {
						t.Fatal(err)
					}
					var machines [][]byte
					for n := range 5 {
						content, err := os.ReadFile(filepath.Join(out, "target-fit-"+strconv.Itoa(n)+".json"))
						if err != nil {
							t.Fatal(err)
						}
						machines = append(machines, content)
					}
					return report.String(), machines, fits
				}
				serialReport, serialMachines, fits := generated(1)
				var constrained, flagged [][]modelcheck.Group
				for n, f := range fits {
					constrained = append(constrained, f.constrained)
					flagged = append(flagged, f.flagged)
					written := flaggedGroups(f.check)
					if diff := cmp.Diff(written, f.flagged); diff != "" {
						t.Fatalf("member %d flags before writing differ from the written file's (-file +header):\n%s", n, diff)
					}
					want := []string{"Model check against the original extract: ok, no flagged groups."}
					if len(written) > 0 {
						want = []string{"Model check against the original extract: flagged; this member cannot support a target-machine claim."}
					}
					for _, g := range written {
						want = append(want, fmt.Sprintf("Flagged: R1  cores=[0] duration=60s depth=%d n=%d k=%d interval=[%d,%d]", g.Depth, g.N, g.K, g.Interval[0], g.Interval[1]))
					}
					var metadata struct {
						Notes []string `json:"notes"`
					}
					if err := json.Unmarshal(serialMachines[n], &metadata); err != nil {
						t.Fatal(err)
					}
					var got []string
					for _, note := range metadata.Notes {
						if strings.HasPrefix(note, "Model check") || strings.HasPrefix(note, "Flagged:") {
							got = append(got, note)
						}
					}
					if diff := cmp.Diff(want, got); diff != "" {
						t.Fatalf("member %d header flags (-want +got):\n%s", n, diff)
					}
				}
				ignore := cmpopts.IgnoreFields(modelcheck.Group{}, "Interval", "MeanP")
				if diff := cmp.Diff(test.constrained, constrained, ignore); diff != "" {
					t.Fatalf("constrained refits (-want +got):\n%s", diff)
				}
				if diff := cmp.Diff(test.flagged, flagged, ignore); diff != "" {
					t.Fatalf("flagged members (-want +got):\n%s", diff)
				}
				var unseeded []int
				for line := range strings.Lines(serialReport) {
					var n int
					if _, err := fmt.Sscanf(line, "Refit %d written unconstrained:", &n); err == nil {
						unseeded = append(unseeded, n)
					}
				}
				if diff := cmp.Diff(test.unseeded, unseeded); diff != "" {
					t.Fatalf("refits reported unconstrained (-want +got):\n%s", diff)
				}
				type evidence struct {
					Trials int
					Loss   float64
				}
				for n, groups := range test.constrained {
					if groups == nil {
						continue
					}
					prediction, base := fits[n].check.Groups[0].MeanP, fits[0].check.Groups[0].MeanP
					if prediction >= base {
						t.Fatalf("refit %d kept the all-facts prediction %g, want below it: %g", n, base, prediction)
					}
					want := evidence{30, -30 * math.Log1p(-prediction)}
					if diff := cmp.Diff(want, evidence{len(fits[n].sample), fits[n].loss}, cmpopts.EquateApprox(0, 0.0001)); diff != "" {
						t.Fatalf("refit %d likelihood on its failure-free resample (-want +got):\n%s", n, diff)
					}
					if !bytes.Contains(serialMachines[n], []byte(`"No failures: default signal weights."`)) {
						t.Fatalf("refit %d keeps a signal mix its failure-free resample never recorded:\n%s", n, serialMachines[n])
					}
				}
				parallelReport, parallelMachines, _ := generated(5)
				if diff := cmp.Diff(serialReport, parallelReport); diff != "" {
					t.Fatalf("parallel report differs from serial (-serial +parallel):\n%s", diff)
				}
				if diff := cmp.Diff(serialMachines, parallelMachines); diff != "" {
					t.Fatalf("parallel machines differ from serial (-serial +parallel):\n%s", diff)
				}
			})
		})
	}
}

func TestGenerateRefusesMissingOrNondecisiveEvidence(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		root := t.TempDir()
		extract := filepath.Join(root, "facts.jsonl.gz")
		out := filepath.Join(root, "machines")
		var output bytes.Buffer
		if _, err := generate(extract, out, 263, 0, 2, &output); err == nil {
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
		if _, err := generate(extract, out, 263, 0, 2, &output); err == nil || err.Error() != "extract has no decisive trials" {
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
				conflict = filepath.Join(out, "target-fit-0.json")
				want = "fit 0 write fitted machine"
				if err := os.MkdirAll(conflict, 0700); err != nil {
					t.Fatal(err)
				}
			} else if err := os.WriteFile(conflict, []byte("preserve"), 0600); err != nil {
				t.Fatal(err)
			}
			var report bytes.Buffer
			if _, err := generate(extract, out, 263, 0, 2, &report); err == nil || !strings.HasPrefix(err.Error(), want+":") || report.Len() != 0 {
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

func encodedMachine(t *testing.T, cfg sim.Config, index int, seed uint64, trials int, loss float64, constrained, flagged []modelcheck.Group) []byte {
	t.Helper()
	content, err := encodeMachine(cfg, index, seed, trials, loss, constrained, flagged)
	if err != nil {
		t.Fatal(err)
	}
	return content
}
