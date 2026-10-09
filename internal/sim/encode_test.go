package sim

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestEncodeMachineRoundTripsEveryField(t *testing.T) {
	t.Parallel()
	want := fullMachineConfig(t)
	content, err := EncodeMachine(want)
	if err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(t.TempDir(), "machine.toml")
	if err := os.WriteFile(path, content, 0o600); err != nil {
		t.Fatal(err)
	}
	got, err := LoadMachine(path)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("round trip changed the config (-want +got):\n%s", diff)
	}
	again, err := EncodeMachine(got)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(string(content), string(again)); diff != "" {
		t.Fatalf("encoding the decoded config differs (-first +second):\n%s", diff)
	}
}

func TestEncodeMachineLeavesDefaultModelFieldsOut(t *testing.T) {
	t.Parallel()
	model := DefaultModel()
	content, err := EncodeMachine(Config{Cores: 2, Model: &model})
	if err != nil {
		t.Fatal(err)
	}
	for _, key := range []string{"crash_mce", "core_local_bank", "onset_s", "signals", "reset", "regime_signals"} {
		if strings.Contains(string(content), key) {
			t.Errorf("default model wrote %s:\n%s", key, content)
		}
	}
	if !strings.Contains(string(content), "onset_boost = 0.0\n") {
		t.Errorf("onset_boost not written with a decimal point:\n%s", content)
	}
}

func TestEncodeMachineRefusesRuntimeOnlyFields(t *testing.T) {
	t.Parallel()
	bios := defaultBIOSContext
	replay, err := NewReplay(bios, []ReplayFact{{Context: bios, Class: journal.TrialClass{Regime: machine.R1, Workload: "work", Cores: []int{0}, DurationS: 60}, Profile: []int{0, 0}, Outcome: journal.OutcomePass, DurationS: 60}})
	if err != nil {
		t.Fatal(err)
	}
	for name, set := range map[string]func(*Config){
		"Seed":   func(c *Config) { c.Seed = 1 },
		"Replay": func(c *Config) { c.Replay = replay },
		"Boots":  func(c *Config) { c.Boots = 1 },
		"Start":  func(c *Config) { c.Start = time.Date(2026, 2, 1, 0, 0, 0, 0, time.UTC) },
	} {
		cfg := Config{Cores: 2}
		set(&cfg)
		content, err := EncodeMachine(cfg)
		if err == nil || !strings.Contains(err.Error(), name+" is runtime-only") {
			t.Errorf("%s: got %q, %v; want a runtime-only error", name, content, err)
		}
	}
}
