package modelcheck

import (
	"compress/gzip"
	"encoding/json"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/tools/trialfacts"
)

func TestModelCheckResetWeights(t *testing.T) {
	for _, kind := range []machine.ResetKind{machine.ResetThermalTrip, machine.ResetPowerLoss} {
		for _, tc := range []struct {
			name   string
			weight float64
		}{
			{"zero", 0},
			{"small-positive", 1e-9},
			{"positive", 1},
		} {
			t.Run(string(kind)+"/"+tc.name, func(t *testing.T) {
				const path = "../bench/testdata/model-ok.toml"
				cfg, err := sim.LoadMachine(path)
				if err != nil {
					t.Fatal(err)
				}
				cfg.Model.Reset = map[machine.ResetKind]float64{machine.ResetWatchdog: 1, kind: tc.weight}
				check, err := Check(path, cfg)
				if tc.weight > 0 {
					if err == nil || check != nil {
						t.Fatalf("nondecisive reset weight accepted: check=%+v err=%v", check, err)
					}
					return
				}
				if err != nil {
					t.Fatal(err)
				}
				if check.Status != "ok" || len(check.Groups) != 1 || check.Groups[0].N != 10 || check.Groups[0].K != 0 {
					t.Fatalf("zero-weight reset changed model check: %+v", check)
				}
			})
		}
	}
}

func TestModelCheckEligibility(t *testing.T) {
	const machinePath = "../bench/testdata/model-ok.toml"
	cfg, err := sim.LoadMachine(machinePath)
	if err != nil {
		t.Fatal(err)
	}
	records, err := trialfacts.Read(filepath.Join(filepath.Dir(machinePath), cfg.Facts))
	if err != nil {
		t.Fatal(err)
	}
	context := machine.BIOSContext{Board: "fixture"}
	for _, tc := range []struct {
		name    string
		n       int
		context *machine.BIOSContext
		machine machine.BIOSContext
		groups  int
	}{
		{"below-ten", 9, nil, machine.BIOSContext{}, 0},
		{"ten", 10, nil, machine.BIOSContext{}, 1},
		{"context-missing", 10, nil, context, 0},
		{"context-mismatch", 10, &context, machine.BIOSContext{Board: "other"}, 0},
		{"context-match", 10, &context, context, 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "facts.jsonl.gz")
			f, err := os.Create(path)
			if err != nil {
				t.Fatal(err)
			}
			defer f.Close()
			gz := gzip.NewWriter(f)
			encoder := json.NewEncoder(gz)
			for _, r := range records[:tc.n] {
				r.Context = tc.context
				if err := encoder.Encode(r); err != nil {
					t.Fatal(err)
				}
			}
			if err := gz.Close(); err != nil {
				t.Fatal(err)
			}
			if err := f.Close(); err != nil {
				t.Fatal(err)
			}
			config := cfg
			config.Facts, config.BIOSContext = path, tc.machine
			check, err := Check(machinePath, config)
			if err != nil {
				t.Fatal(err)
			}
			if check.Status != "ok" || len(check.Groups) != tc.groups {
				t.Fatalf("eligibility check = %+v; want %d groups", check, tc.groups)
			}
			if tc.groups == 1 && (check.Groups[0].N != tc.n || check.Groups[0].K != 0) {
				t.Fatalf("eligible group = %+v", check.Groups[0])
			}
		})
	}
}

func TestCheckRefusesUnreadableExtract(t *testing.T) {
	path := filepath.Join(t.TempDir(), "machine.toml")
	got, err := Check(path, sim.Config{Facts: "missing.jsonl.gz"})
	if got != nil || !errors.Is(err, fs.ErrNotExist) || !strings.Contains(err.Error(), "missing.jsonl.gz") {
		t.Fatalf("missing extract: result=%v err=%v", got, err)
	}
}

func TestCheckRecordsRefusesInvalidMachine(t *testing.T) {
	got, err := CheckRecords("machine.toml", "facts.jsonl.gz", sim.Config{Cores: 3}, nil)
	if got != nil || err == nil || !strings.Contains(err.Error(), "must be even and at least 2") {
		t.Fatalf("invalid machine: result=%v err=%v", got, err)
	}
}

func TestCheckerRefusesMalformedTrials(t *testing.T) {
	valid := trialfacts.Record{Session: "extract", Seq: 7, Kind: facts.TrialFact, Outcome: journal.OutcomePass, Profile: []int{0, 0}, Class: facts.Class{Regime: machine.R1, Cores: []int{0}, DurationS: 60}}
	for _, tc := range []struct {
		name   string
		change func(*trialfacts.Record)
		want   string
	}{
		{"profile size", func(r *trialfacts.Record) { r.Profile = []int{0} }, "invalid trial fact extract:7"},
		{"empty loaded", func(r *trialfacts.Record) { r.Class.Cores = nil }, "invalid trial fact extract:7"},
		{"zero duration", func(r *trialfacts.Record) { r.Class.DurationS = 0 }, "invalid trial fact extract:7"},
		{"negative duration", func(r *trialfacts.Record) { r.Class.DurationS = -1 }, "invalid trial fact extract:7"},
		{"negative core", func(r *trialfacts.Record) { r.Class.Cores = []int{-1} }, "invalid loaded core in fact extract:7"},
		{"core out of range", func(r *trialfacts.Record) { r.Class.Cores = []int{2} }, "invalid loaded core in fact extract:7"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			r := valid
			tc.change(&r)
			got, err := NewChecker(sim.Config{Cores: 2}, []trialfacts.Record{r})
			if got != nil || err == nil || err.Error() != tc.want {
				t.Fatalf("got %v %v; want %q", got, err, tc.want)
			}
		})
	}
}

func TestCheckerCountsIdleWithoutInventingExposure(t *testing.T) {
	model := sim.DefaultModel()
	model.NearLimitRate = 0
	cfg := sim.Config{Model: &model}
	var records []trialfacts.Record
	for range 10 {
		records = append(records, trialfacts.Record{Kind: facts.TrialFact, Outcome: journal.OutcomePass, Profile: make([]int, 16), Class: facts.Class{Regime: machine.R1, Cores: []int{0}, DurationS: 60}})
	}
	records = append(records, trialfacts.Record{Kind: facts.IdleFact}, trialfacts.Record{Kind: facts.IdleFact}, trialfacts.Record{Kind: facts.TrialFact, Outcome: journal.Outcome("inconclusive")}, trialfacts.Record{Kind: facts.Kind("other")})
	check, err := CheckRecords("machine", "extract", cfg, records)
	if err != nil {
		t.Fatal(err)
	}
	want := &Result{Machine: "machine", Extract: "extract", Status: "ok", IdleFailures: 2, Groups: []Group{{Kind: facts.TrialFact, Class: facts.Class{Regime: machine.R1, Cores: []int{0}, DurationS: 60}, N: 10, Interval: [2]int{0, 0}}}}
	if diff := cmp.Diff(want, check); diff != "" {
		t.Fatal(diff)
	}
}
