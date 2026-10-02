package modelcheck

import (
	"compress/gzip"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

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
