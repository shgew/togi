package sim

import (
	"math"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"
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

// encodeRoundTrip encodes cfg, checks that LoadMachine reads the file back as cfg and that encoding the result
// writes the same file, and returns the file.
func encodeRoundTrip(t *testing.T, cfg Config) string {
	t.Helper()
	content, err := EncodeMachine(cfg)
	if err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(t.TempDir(), "machine.toml")
	if err := os.WriteFile(path, content, 0o600); err != nil {
		t.Fatal(err)
	}
	got, err := LoadMachine(path)
	if err != nil {
		t.Fatalf("%v\n%s", err, content)
	}
	if diff := cmp.Diff(cfg, got, cmpopts.EquateNaNs()); diff != "" {
		t.Fatalf("round trip changed the config (-want +got):\n%s", diff)
	}
	again, err := EncodeMachine(got)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(string(content), string(again)); diff != "" {
		t.Fatalf("encoding the decoded config differs (-first +second):\n%s", diff)
	}
	return string(content)
}

func TestEncodeMachineRoundTripsControlCharacterStrings(t *testing.T) {
	t.Parallel()
	cfg := fullMachineConfig(t)
	cfg.Facts = "\b\f\n\r\t\"\\"
	cfg.BIOSContext.Board = "board\a"
	cfg.Limits[0].Workload["w\v"] = -22
	for _, trial := range []string{"bell\a", "tab\v", "\x01\x1f\x7f", `literal\a\v`, "é✓\u2028"} {
		cfg.Script[trial] = Outcome{Signal: machine.Crash, Core: 1}
	}
	content := encodeRoundTrip(t, cfg)
	for _, want := range []string{
		`facts = "\b\f\n\r\t\"\\"` + "\n",
		`board = "board\u0007"` + "\n",
		`"w\u000b" = -22`,
		`trial = "bell\u0007"` + "\n",
		`trial = "tab\u000b"` + "\n",
		`trial = "\u0001\u001f\u007f"` + "\n",
		`trial = "literal\\a\\v"` + "\n",
		`trial = "é✓\u2028"` + "\n",
	} {
		if !strings.Contains(content, want) {
			t.Errorf("missing %s in:\n%s", want, content)
		}
	}
}

func TestEncodeMachineRoundTripsLargeWholeFloats(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name string
		set  func(*Config)
		want string
	}{
		{"reset weight at the safe integer limit", func(c *Config) { c.Model.Reset[machine.ResetWatchdog] = 1<<53 - 1 }, `"watchdog" = 9007199254740991`},
		{"reset weight past the safe integer limit", func(c *Config) { c.Model.Reset[machine.ResetWatchdog] = 1 << 53 }, `"watchdog" = 9007199254740992.0`},
		{"reset weight 1e16", func(c *Config) { c.Model.Reset[machine.ResetWatchdog] = 1e16 }, `"watchdog" = 10000000000000000.0`},
		{"small onset_s", func(c *Config) { c.Model.OnsetS = 50 }, "onset_s = 50\n"},
		{"onset_s at the negative safe integer limit", func(c *Config) { c.Model.OnsetS = -(1<<53 - 1) }, "onset_s = -9007199254740991\n"},
		{"onset_s 1e16", func(c *Config) { c.Model.OnsetS = 1e16 }, "onset_s = 10000000000000000.0\n"},
		{"onset_s -1e16", func(c *Config) { c.Model.OnsetS = -1e16 }, "onset_s = -10000000000000000.0\n"},
		{"at_s just below 1e17", func(c *Config) { c.Script["0001"] = Outcome{AtS: 99999999999999984, Core: 2} }, "at_s = 99999999999999984.0\n"},
		{"ccd log_rate -1e16", func(c *Config) { c.CCD.LogRate = -1e16 }, "log_rate = -10000000000000000.0\n"},
		{"core flat 1e16", func(c *Config) { c.Limits[1].Flat = 1e16 }, "flat = 10000000000000000.0\n"},
		{"onset_boost 1e16", func(c *Config) { c.Model.OnsetBoost = 1e16 }, "onset_boost = 10000000000000000.0\n"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			cfg := fullMachineConfig(t)
			tc.set(&cfg)
			if content := encodeRoundTrip(t, cfg); !strings.Contains(content, tc.want) {
				t.Errorf("missing %s in:\n%s", tc.want, content)
			}
		})
	}
}

func TestEncodeMachineRoundTripsNonFiniteFloats(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name string
		set  func(*Config)
		want string
	}{
		{"onset_s inf", func(c *Config) { c.Model.OnsetS = math.Inf(1) }, "onset_s = inf\n"},
		{"onset_s -inf", func(c *Config) { c.Model.OnsetS = math.Inf(-1) }, "onset_s = -inf\n"},
		{"past_limit_rate nan", func(c *Config) { c.Model.PastLimitRate = math.NaN() }, "past_limit_rate = nan\n"},
		{"onset_boost -inf", func(c *Config) { c.Model.OnsetBoost = math.Inf(-1) }, "onset_boost = -inf\n"},
		{"reset weight inf", func(c *Config) { c.Model.Reset[machine.ResetWatchdog] = math.Inf(1) }, `"watchdog" = inf`},
		{"at_s nan", func(c *Config) { c.Script["0001"] = Outcome{AtS: math.NaN(), Core: 2} }, "at_s = nan\n"},
		{"core flat inf", func(c *Config) { c.Limits[1].Flat = math.Inf(1) }, "flat = inf\n"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			cfg := fullMachineConfig(t)
			tc.set(&cfg)
			if content := encodeRoundTrip(t, cfg); !strings.Contains(content, tc.want) {
				t.Errorf("missing %s in:\n%s", tc.want, content)
			}
		})
	}
}
