package config

import (
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func TestLoad(t *testing.T) {
	t.Parallel()
	partial := Default()
	partial.Durations.SearchTrialS = 60
	shortTrial := Default()
	shortTrial.Durations.ShortTrialS = 86400
	evidence := Default()
	evidence.Evidence.Miss, evidence.Evidence.Rate = 0.001, 0.25
	offsets := Default()
	offsets.StartOffsets = map[int]int{3: -10}
	soloLimits := Default()
	soloLimits.StartOffsets = map[int]int{2: -5}
	soloLimits.CandidateSoloLimits = map[int]int{3: -36}
	cycle := Default()
	allCore := Default()
	allCore.Durations.CheckingAllCoreS = 4
	cycle.Checking.Cycle = []machine.Regime{machine.R7}
	user := Default()
	user.BackendUser = "togi-trial"

	tests := []struct {
		name    string
		content string
		want    Config
		reject  bool
		wantErr string
	}{
		{name: "empty object", content: `{}`, want: Default()},
		{name: "empty file", content: "", reject: true},
		{name: "backend user", content: `{"backend_user":"togi-trial"}`, want: user},
		{name: "backend user wrong type", content: `{"backend_user":1001}`, reject: true},
		{name: "partial file", content: `{"durations":{"search_trial_s":60}}`, want: partial},
		{name: "start offset", content: `{"start_offsets":{"3":-10}}`, want: offsets},
		{name: "candidate solo limit beside another core's start offset", content: `{"start_offsets":{"2":-5},"candidate_solo_limits":{"3":-36}}`, want: soloLimits},
		{name: "candidate solo limit and start offset for one core", content: `{"start_offsets":{"3":-30},"candidate_solo_limits":{"3":-36}}`, reject: true},
		{name: "candidate solo limit below floor", content: `{"candidate_solo_limits":{"3":-51}}`, reject: true},
		{name: "cycle replaced", content: `{"checking":{"cycle":["R7"]}}`, want: cycle},
		{name: "shortest all-core duration", content: `{"durations":{"checking_all_core_s":4}}`, want: allCore},
		{name: "longest short trial", content: `{"durations":{"short_trial_s":86400}}`, want: shortTrial},
		{name: "zero short trial", content: `{"durations":{"short_trial_s":0}}`, reject: true},
		{name: "short trial beyond a day", content: `{"durations":{"short_trial_s":86401}}`, reject: true},
		{name: "evidence overridden", content: `{"evidence":{"miss":0.001,"rate":0.25}}`, want: evidence},
		{name: "zero miss", content: `{"evidence":{"miss":0}}`, wantErr: "evidence.miss = 0: must be within (0, 1)"},
		{name: "one miss", content: `{"evidence":{"miss":1}}`, wantErr: "evidence.miss = 1: must be within (0, 1)"},
		{name: "zero rate", content: `{"evidence":{"rate":0}}`, wantErr: "evidence.rate = 0: must be within (0, 1)"},
		{name: "one rate", content: `{"evidence":{"rate":1}}`, wantErr: "evidence.rate = 1: must be within (0, 1)"},
		{name: "removed failure rate", content: `{"evidence":{"failure_rate":0.1}}`, reject: true},
		{name: "removed significance", content: `{"evidence":{"significance":0.3}}`, reject: true},
		{name: "too many trials", content: `{"evidence":{"rate":1e-300}}`, wantErr: "evidence: miss 0.05 and rate 1e-300 need more than 1000 trials per step"},
		{name: "removed confirmation key takes precedence", content: `{"durations":{"confirmation_trial_s":300,"bogus":1}}`, wantErr: "durations.confirmation_trial_s was removed in togi 0.5.0: confirmation no longer exists; delete the key"},
		{name: "removed confirmation key even when null", content: `{"durations":{"confirmation_trial_s":null}}`, wantErr: "durations.confirmation_trial_s was removed in togi 0.5.0: confirmation no longer exists; delete the key"},
		{name: "all-core duration too short to split", content: `{"durations":{"checking_all_core_s":3}}`, reject: true},
		{name: "unknown top-level key", content: `{"bogus":1}`, reject: true},
		{name: "unknown nested key", content: `{"durations":{"search_s":1}}`, reject: true},
		{name: "unknown checking key", content: `{"checking":{"bogus":1}}`, reject: true},
		{name: "unknown evidence key", content: `{"evidence":{"bogus":1}}`, reject: true},
		{name: "unknown dead-end key", content: `{"dead_ends":{"bogus":1}}`, reject: true},
		{name: "unknown backend key", content: `{"backends":{"bogus":"/bin/backend"}}`, reject: true},
		{name: "old checking key rejected", content: `{"checking":{"lap":["R7"]}}`, reject: true},
		{name: "old duration key rejected", content: `{"durations":{"start_s":120}}`, reject: true},
		{name: "syntax error", content: `{"durations":`, reject: true},
		{name: "trailing object", content: `{} {}`, reject: true},
		{name: "trailing garbage", content: `{} invalid`, reject: true},
		{name: "duplicate top-level name", content: `{"backend_user":"first","backend_user":"second"}`, reject: true},
		{name: "duplicate nested name", content: `{"durations":{"search_trial_s":60,"search_trial_s":90}}`, reject: true},
		{name: "duplicate core name", content: `{"start_offsets":{"3":-10,"3":-20}}`, reject: true},
		{name: "wrong type", content: `{"durations":{"search_trial_s":"90"}}`, reject: true},
		{name: "positive offset", content: `{"start_offsets":{"3":1}}`, reject: true},
		{name: "offset below floor", content: `{"start_offsets":{"3":-51}}`, reject: true},
		{name: "core not a number", content: `{"start_offsets":{"x":-1}}`, reject: true},
		{name: "negative core", content: `{"start_offsets":{"-1":-1}}`, reject: true},
		{name: "core not canonical", content: `{"start_offsets":{"03":-1}}`, reject: true},
		{name: "zero duration", content: `{"durations":{"checking_idle_s":0}}`, reject: true},
		{name: "empty cycle", content: `{"checking":{"cycle":[]}}`, reject: true},
		{name: "cycle not a regime", content: `{"checking":{"cycle":["R1","R8"]}}`, reject: true},
		{name: "zero threshold", content: `{"dead_ends":{"stray_crashes_in_a_row":0}}`, reject: true},
		{name: "relative backend", content: `{"backends":{"mprime":"bin/mprime"}}`, reject: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			path := filepath.Join(t.TempDir(), "config.json")
			if err := os.WriteFile(path, []byte(tt.content), 0o644); err != nil {
				t.Fatal(err)
			}
			got, err := Load(path)
			if tt.wantErr != "" {
				if err == nil || !strings.Contains(err.Error(), tt.wantErr) {
					t.Fatalf("Load error = %v, want it to contain %q", err, tt.wantErr)
				}
				return
			}
			if tt.reject {
				if err == nil {
					t.Fatal("Load accepted invalid config")
				}
				return
			}
			if err != nil {
				t.Fatalf("Load: %v", err)
			}
			if diff := cmp.Diff(tt.want, got); diff != "" {
				t.Fatalf("Load mismatch (-want +got):\n%s", diff)
			}
		})
	}
}

func TestLoadMissingFile(t *testing.T) {
	t.Parallel()
	_, err := Load(filepath.Join(t.TempDir(), "missing.json"))
	if !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("Load error = %v, want fs.ErrNotExist", err)
	}
}

func TestEvidenceTrials(t *testing.T) {
	t.Parallel()
	for _, tt := range []struct {
		name     string
		evidence Evidence
		want     int
	}{
		{"default", Evidence{Miss: 0.05, Rate: 0.5}, 5},
		{"smaller miss", Evidence{Miss: 0.001, Rate: 0.5}, 10},
		{"smaller rate", Evidence{Miss: 0.05, Rate: 0.25}, 11},
	} {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			if diff := cmp.Diff(tt.want, tt.evidence.Trials()); diff != "" {
				t.Fatalf("Trials mismatch (-want +got):\n%s", diff)
			}
		})
	}
}

func TestDefaultCycle(t *testing.T) {
	t.Parallel()
	want := []machine.Regime{machine.R7, machine.R7, machine.R7, machine.R2, machine.R2, machine.R2, machine.R6, machine.R5, machine.R1, machine.R1, machine.R1, machine.R3, machine.R3, machine.R3, machine.R4, machine.R4, machine.R4, machine.R6}
	if diff := cmp.Diff(want, Default().Checking.Cycle); diff != "" {
		t.Fatalf("default cycle mismatch (-want +got):\n%s", diff)
	}
}
