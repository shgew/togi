package trialfacts

import (
	"compress/gzip"
	"encoding/json"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
)

func TestExtractsReplaySelectsTrialEvidence(t *testing.T) {
	context := machine.BIOSContext{BIOSVersion: "fixture"}
	other := machine.BIOSContext{BIOSVersion: "other"}
	class := facts.Class{Regime: machine.R1, Workload: "fixture", Cores: []int{0}, DurationS: 90}
	dir := t.TempDir()
	path := filepath.Join(dir, "facts.gz")
	f, err := os.Create(path)
	if err != nil {
		t.Fatal(err)
	}
	gz := gzip.NewWriter(f)
	enc := json.NewEncoder(gz)
	for _, r := range []Record{
		{Session: "s1", Seq: 4, Trial: "1", Kind: facts.TrialFact, Context: &context, Class: class, Profile: []int{-10, 0}, Outcome: journal.OutcomePass, DurationS: 90},
		{Session: "s1", Seq: 5, Kind: facts.IdleFact, Context: &context, Class: facts.Class{Regime: machine.R6, Cores: []int{0, 1}}, Condition: machine.Together, Profile: []int{-20, 0}, Outcome: journal.OutcomeFailure, Signal: machine.Crash},
		{Session: "s2", Seq: 4, Trial: "1", Kind: facts.TrialFact, Class: class, Profile: []int{-30, 0}, Outcome: journal.OutcomePass, DurationS: 90},
		{Session: "s3", Seq: 4, Trial: "1", Kind: facts.TrialFact, Context: &other, Class: class, Profile: []int{-40, 0}, Outcome: journal.OutcomePass, DurationS: 90},
	} {
		if err := enc.Encode(r); err != nil {
			t.Fatal(err)
		}
	}
	if err := gz.Close(); err != nil {
		t.Fatal(err)
	}
	if err := f.Close(); err != nil {
		t.Fatal(err)
	}
	for _, extract := range []string{"facts.gz", path} {
		cfg := sim.Config{Seed: 1, Cores: 2, BIOSContext: context, Facts: extract}
		cfg.Replay, err = Extracts{}.Replay(filepath.Join(dir, "machine.toml"), cfg)
		if err != nil {
			t.Fatal(err)
		}
		m, err := sim.New(cfg)
		if err != nil {
			t.Fatal(err)
		}
		spec := machine.TrialSpec{Regime: machine.R1, Workload: machine.Workload{ID: "fixture"}, Cores: []int{0}, Duration: 90 * time.Second}
		for _, offset := range []int{-10, -20, -30, -40} {
			if got := m.HasRealAnswer([]int{offset, 0}, spec); got != (offset == -10) {
				t.Fatalf("extract=%s offset=%d answer=%v", extract, offset, got)
			}
		}
	}
	for _, tc := range []struct {
		name string
		cfg  sim.Config
		want string
	}{
		{"no-extract", sim.Config{BIOSContext: context}, "machine has no facts extract"},
		{"missing-extract", sim.Config{BIOSContext: context, Facts: "missing.gz"}, "open extract"},
		{"no-context", sim.Config{Facts: path}, "BIOS context is required"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			replay, err := Extracts{}.Replay(filepath.Join(dir, "machine.toml"), tc.cfg)
			if replay != nil || err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("Replay=%v, %v; want %s", replay, err, tc.want)
			}
		})
	}
}

func TestExtractsReadEachExtractOnce(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "facts.gz")
	f, err := os.Create(path)
	if err != nil {
		t.Fatal(err)
	}
	gz := gzip.NewWriter(f)
	want := []Record{{Session: "s1", Seq: 4, Kind: facts.TrialFact, Profile: []int{-10, 0}, Outcome: journal.OutcomePass}}
	if err := json.NewEncoder(gz).Encode(want[0]); err != nil {
		t.Fatal(err)
	}
	if err := gz.Close(); err != nil {
		t.Fatal(err)
	}
	if err := f.Close(); err != nil {
		t.Fatal(err)
	}
	extracts := Extracts{}
	for _, machinePath := range []string{filepath.Join(dir, "first.toml"), filepath.Join(dir, "second.toml")} {
		got, records, err := extracts.Load(machinePath, sim.Config{Facts: "facts.gz"})
		if err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff(path, got); diff != "" {
			t.Fatal(diff)
		}
		if diff := cmp.Diff(want, records); diff != "" {
			t.Fatal(diff)
		}
		if err := os.Remove(path); err != nil && !errors.Is(err, fs.ErrNotExist) {
			t.Fatal(err)
		}
	}
}
