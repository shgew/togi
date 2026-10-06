package main

import (
	"bytes"
	"compress/gzip"
	"encoding/json"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/tools/modelcheck"
	"github.com/shgew/togi/tools/trialfacts"
)

func anchorRecords() []trialfacts.Record {
	context := machine.BIOSContext{Board: "fixture", BIOSVersion: "A", CPUModel: "Zen 5 fixture", Microcode: "0x1", BoostLimitMHz: 5600}
	var records []trialfacts.Record
	for i := range 20 {
		profile := make([]int, 16)
		for core := range profile {
			profile[core] = -20
		}
		r := trialfacts.Record{Session: "20260101T000000Z", Seq: i + 1, Kind: facts.TrialFact, Outcome: journal.OutcomePass, Profile: profile, Context: &context, Class: facts.Class{Regime: machine.R7, Workload: machine.PickWorkload(machine.R7, 0).ID, Cores: []int{0, 1, 2, 3, 4, 5, 6, 7}, DurationS: 120}, DurationS: 120}
		if i >= 10 {
			r.Outcome, r.Signal, r.DurationS = journal.OutcomeFailure, machine.Crash, 60
		}
		records = append(records, r)
	}
	return records
}

func anchorConfig(records []trialfacts.Record) (sim.Config, float64) {
	cfg := initialConfig(records)
	cfg.Joints, cfg.CCD = nil, nil
	cfg.Model.PastLimitRate, cfg.Model.NearLimitRate = 0, 0
	background := .0001234567890123456
	cfg.SharedVoltage = &sim.SharedVoltage{IdleV: .8, MarginV: .006, Rate: (math.Ln2/120 - background) / 8, BackgroundRate: background, PowerLimitW: 192, ThermalLimitW: 192, Workload: make(map[string]sim.VoltageWorkload)}
	for _, workload := range machine.Workloads(machine.R7) {
		w := sim.VoltageWorkload{ReferenceMHz: 5240, FullMHz: [2]float64{5240, 5240}, IdleGainMHz: float64(len(workload.ID)), WattsPerCore: 13, Core: make([]sim.VoltageCore, 16)}
		for core := range w.Core {
			w.Core[core] = sim.VoltageCore{BaseV: 1.2, ThresholdV: 1.2 - .0036*20, CountV: .0036, ClockVPer100MHz: .033123456789012345 + float64(core)*.0001, ThresholdClockVPer100MHz: .012345678901234567 + float64(core)*.0001, Signals: map[machine.Signal]float64{machine.Crash: 1, machine.ComputationError: .25}}
		}
		cfg.SharedVoltage.Workload[workload.ID] = w
	}
	return cfg, 20 * math.Ln2
}

func writeAnchorExtract(t *testing.T, path string, records []trialfacts.Record) {
	t.Helper()
	var compressed bytes.Buffer
	gz := gzip.NewWriter(&compressed)
	encoder := json.NewEncoder(gz)
	for _, r := range records {
		if err := encoder.Encode(r); err != nil {
			t.Fatal(err)
		}
	}
	if err := gz.Close(); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, compressed.Bytes(), 0o600); err != nil {
		t.Fatal(err)
	}
}

func TestSharedVoltageAnchorDispatchAndConflictingFlags(t *testing.T) {
	for _, tc := range []struct {
		args []string
		code int
		text string
	}{
		{[]string{"--shared-voltage-in-sample", "--forward-only"}, 2, "cannot be combined with --forward-only"},
		{[]string{"--shared-voltage-in-sample", "--forward-only=false"}, 2, "cannot be combined with --forward-only"},
		{[]string{"--shared-voltage-in-sample", "--bootstrap", "0"}, 2, "cannot be combined with --bootstrap"},
		{[]string{"--shared-voltage-in-sample", "--seed", "263"}, 2, "cannot be combined with --seed"},
		{[]string{"--shared-voltage-in-sample", "--seal", "0"}, 2, "cannot be combined with --seal"},
		{[]string{"--shared-voltage-in-sample", "extra"}, 2, "require no positional arguments"},
		{[]string{"--help"}, 0, "shared-voltage-in-sample"},
		{[]string{"--bootstrap", "-1"}, 2, "nonnegative --bootstrap"},
		{nil, 1, "open extract"},
		{[]string{"--shared-voltage-in-sample=false"}, 1, "open extract"},
		{[]string{"--forward-only"}, 1, "open extract"},
	} {
		t.Run(strings.Join(tc.args, " "), func(t *testing.T) {
			var stdout, stderr bytes.Buffer
			fitter := func([]trialfacts.Record) (sim.Config, float64) {
				t.Fatal("unexpected shared-voltage fit")
				return sim.Config{}, 0
			}
			args := append([]string{"--facts", filepath.Join(t.TempDir(), "missing.gz")}, tc.args...)
			code := runWithSharedVoltageFit(args, &stdout, &stderr, fitter)
			if code != tc.code || !strings.Contains(stdout.String()+stderr.String(), tc.text) {
				t.Fatalf("code=%d stdout=%q stderr=%q; want %d and %q", code, stdout.String(), stderr.String(), tc.code, tc.text)
			}
		})
	}
}

func TestSharedVoltageAnchorCheckedDeterministicAndReplayable(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		root := t.TempDir()
		extract, out := filepath.Join(root, "facts.gz"), filepath.Join(root, "machines")
		records := anchorRecords()
		idle := records[0]
		idle.Kind = facts.IdleFact
		inconclusive := records[0]
		inconclusive.Outcome = journal.OutcomeInconclusive
		writeAnchorExtract(t, extract, append(append(records, idle), inconclusive))
		var firstContent []byte
		var firstReport string
		for attempt := range 2 {
			var stdout, stderr bytes.Buffer
			calls := 0
			fitter := func(got []trialfacts.Record) (sim.Config, float64) {
				calls++
				if diff := cmp.Diff(records, got); diff != "" {
					t.Fatalf("fit did not receive all decisive facts: %s", diff)
				}
				return anchorConfig(got)
			}
			if code := runWithSharedVoltageFit([]string{"--shared-voltage-in-sample", "--facts", extract, "--out", out}, &stdout, &stderr, fitter); code != 0 || calls != 1 || stderr.Len() != 0 {
				t.Fatalf("code=%d calls=%d stderr=%s", code, calls, stderr.String())
			}
			path := filepath.Join(out, "target-shared-voltage.toml")
			content, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			cfg, err := sim.LoadMachine(path)
			if err != nil {
				t.Fatal(err)
			}
			want, loss := anchorConfig(records)
			if diff := cmp.Diff(want.SharedVoltage, cfg.SharedVoltage); diff != "" {
				t.Fatalf("shared parameters lost precision/order: %s", diff)
			}
			if diff := cmp.Diff(want.Limits, cfg.Limits); diff != "" {
				t.Fatal(diff)
			}
			if cfg.Facts != "../facts.gz" || cfg.BIOSContext != *records[0].Context {
				t.Fatalf("lost replay provenance: %+v", cfg)
			}
			extracts := trialfacts.Extracts{}
			check, err := modelcheck.Check(path, cfg, extracts)
			if err != nil || check.Status != "ok" || len(check.Groups) != 1 || check.Groups[0].N != 20 || check.Groups[0].K != 10 || check.IdleFailures != 1 {
				t.Fatalf("original-extract check: %+v, %v", check, err)
			}
			cfg.Replay, err = extracts.Replay(path, cfg)
			if err != nil {
				t.Fatal(err)
			}
			m, err := sim.New(cfg)
			if err != nil {
				t.Fatal(err)
			}
			r := records[0]
			spec := machine.TrialSpec{Regime: r.Class.Regime, Workload: machine.Workload{ID: r.Class.Workload}, Cores: r.Class.Cores, Duration: 120 * time.Second}
			if !m.HasRealAnswer(r.Profile, spec) {
				t.Fatal("generated provenance cannot answer matching facts")
			}
			for _, text := range []string{"IN-SAMPLE", "NOT forward-validated", "2026-10-04 #306", "threshold_clock_v_per_100mhz", fmt.Sprintf("background_rate = %.17g", want.SharedVoltage.BackgroundRate)} {
				if !bytes.Contains(content, []byte(text)) {
					t.Fatalf("missing anchor header/parameter %q", text)
				}
			}
			for _, text := range []string{"Whole-trial bootstrap", "layered R7 CCD joints", "\nreplay ="} {
				if bytes.Contains(content, []byte(text)) {
					t.Fatalf("misleading or unsupported anchor content %q", text)
				}
			}
			if !strings.Contains(stdout.String(), fmt.Sprintf("raw total in-sample log loss %.17g", loss)) || !strings.Contains(stdout.String(), "R7 in-sample (record only): trials=20 observed=10 predicted=") || !strings.Contains(stdout.String(), "ok (1 eligible groups; 1 idle failures") {
				t.Fatalf("missing raw in-sample diagnostics: %s", stdout.String())
			}
			candidate := fmt.Sprintf("Shared-voltage candidate (record only): rate=%.17g/s margin_v=%.17g background_rate=%.17g/s", want.SharedVoltage.Rate, want.SharedVoltage.MarginV, want.SharedVoltage.BackgroundRate)
			if !strings.Contains(stdout.String(), candidate) {
				t.Fatalf("missing candidate parameter diagnostics: %s", stdout.String())
			}
			_, r7Report, _ := strings.Cut(stdout.String(), "R7 in-sample (record only): ")
			var trials, observed int
			var predicted, r7Loss, r7MeanLoss float64
			fields, err := fmt.Sscanf(r7Report, "trials=%d observed=%d predicted=%f raw_log_loss=%f log_loss/trial=%f", &trials, &observed, &predicted, &r7Loss, &r7MeanLoss)
			if err != nil || fields != 5 || trials != 20 || observed != 10 || math.Abs(predicted-10) > 1e-10 || math.Abs(r7Loss-loss) > 1e-10 || math.Abs(r7MeanLoss-math.Ln2) > 1e-10 {
				t.Fatalf("incorrect raw R7 diagnostics: %q (%v)", r7Report, err)
			}
			if attempt == 0 {
				firstContent, firstReport = content, stdout.String()
			} else if !bytes.Equal(firstContent, content) || firstReport != stdout.String() {
				t.Fatal("fixed inputs changed anchor or deterministic report")
			}
			if _, err := os.Stat(filepath.Join(out, "target-fit-0.toml")); !os.IsNotExist(err) {
				t.Fatalf("anchor mode generated legacy fit: %v", err)
			}
		}
	})
}

func TestSharedVoltageAnchorFlaggedFitNeverWrites(t *testing.T) {
	for _, existing := range []bool{false, true} {
		t.Run(fmt.Sprint(existing), func(t *testing.T) {
			root := t.TempDir()
			extract, out := filepath.Join(root, "facts.gz"), filepath.Join(root, "machines")
			writeAnchorExtract(t, extract, anchorRecords())
			path := filepath.Join(out, "target-shared-voltage.toml")
			if existing {
				if err := os.MkdirAll(out, 0o700); err != nil {
					t.Fatal(err)
				}
				if err := os.WriteFile(path, []byte("preserve anchor"), 0o600); err != nil {
					t.Fatal(err)
				}
			}
			fitter := func(records []trialfacts.Record) (sim.Config, float64) {
				cfg, loss := anchorConfig(records)
				cfg.SharedVoltage.BackgroundRate = 0
				for id, w := range cfg.SharedVoltage.Workload {
					for core := range w.Core {
						w.Core[core].ThresholdV = .8
					}
					cfg.SharedVoltage.Workload[id] = w
				}
				return cfg, loss
			}
			var stdout, stderr bytes.Buffer
			code := runWithSharedVoltageFit([]string{"--shared-voltage-in-sample", "--facts", extract, "--out", out}, &stdout, &stderr, fitter)
			if code != 1 || !strings.Contains(stderr.String(), "anchor not written") || !strings.Contains(stdout.String(), "flagged (1 eligible groups") || !strings.Contains(stdout.String(), "cores=[0 1 2 3 4 5 6 7] duration=120s depth=-20 n=20 k=10 interval=[0,0] mean_p=") {
				t.Fatalf("code=%d stdout=%s stderr=%s", code, stdout.String(), stderr.String())
			}
			if !strings.Contains(stdout.String(), "All trials in-sample (record only): trials=20 raw total in-sample log loss ") || !strings.Contains(stdout.String(), "R7 in-sample (record only): trials=20 observed=10 predicted=") || strings.Count(stdout.String(), "log_loss/trial=") != 2 || strings.Contains(stdout.String(), "Wrote IN-SAMPLE anchor:") {
				t.Fatalf("flagged fit lost record-only scores or claimed generation: %s", stdout.String())
			}
			if existing {
				content, err := os.ReadFile(path)
				if err != nil || string(content) != "preserve anchor" {
					t.Fatalf("flagged fit replaced anchor: %q, %v", content, err)
				}
			} else if _, err := os.Stat(out); !os.IsNotExist(err) {
				t.Fatalf("flagged fit created output directory: %v", err)
			}
		})
	}
}

func TestSharedVoltageAnchorValidatesBeforeFitting(t *testing.T) {
	for _, tc := range []struct {
		name string
		edit func([]trialfacts.Record)
		text string
	}{
		{"core count", func(records []trialfacts.Record) {
			for i := range records {
				records[i].Profile = []int{-20, -20}
				records[i].Class.Cores = []int{0, 1}
			}
		}, "requires 16 cores"},
		{"missing BIOS", func(records []trialfacts.Record) {
			for i := range records {
				records[i].Context = nil
			}
		}, "requires BIOS context"},
		{"partial BIOS", func(records []trialfacts.Record) {
			context := *records[0].Context
			context.Microcode = ""
			for i := range records {
				records[i].Context = &context
			}
		}, "requires BIOS context with every field set"},
		{"mixed BIOS", func(records []trialfacts.Record) {
			context := *records[0].Context
			context.Board = "other"
			records[1].Context = &context
		}, "mixed BIOS contexts"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			root := t.TempDir()
			extract, out := filepath.Join(root, "facts.gz"), filepath.Join(root, "machines")
			records := anchorRecords()
			tc.edit(records)
			writeAnchorExtract(t, extract, records)
			fitter := func([]trialfacts.Record) (sim.Config, float64) {
				t.Fatal("invalid facts reached shared-voltage fit")
				return sim.Config{}, 0
			}
			var report bytes.Buffer
			if err := generateSharedVoltageAnchor(extract, out, &report, fitter); err == nil || !strings.Contains(err.Error(), tc.text) {
				t.Fatalf("invalid facts: %v", err)
			}
			if _, err := os.Stat(out); !os.IsNotExist(err) {
				t.Fatalf("invalid facts created output: %v", err)
			}
		})
	}
}
