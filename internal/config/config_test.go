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
	tolerance := Default()
	tolerance.Evidence.FailureRate, tolerance.Evidence.Significance = 0.1, 0.3

	tests := []struct {
		name    string
		content string
		want    Config
		wantErr string
	}{
		{name: "empty file", content: "", want: Default()},
		{name: "backend user", content: "backend_user = \"togi-trial\"\n", want: user},
		{name: "backend user wrong type", content: "backend_user = 1001\n", wantErr: "backend_user"},
		{name: "partial file", content: "[durations]\nsearch_trial_s = 60\n", want: partial},
		{name: "start offset", content: "[start_offsets]\n3 = -10\n", want: offsets},
		{name: "candidate solo limit beside another core's start offset", content: "[start_offsets]\n2 = -5\n[candidate_solo_limits]\n3 = -36\n", want: soloLimits},
		{name: "candidate solo limit and start offset for one core", content: "[start_offsets]\n3 = -30\n[candidate_solo_limits]\n3 = -36\n", wantErr: `start_offsets."3" and candidate_solo_limits."3": set at most one per core`},
		{name: "candidate solo limit below floor", content: "[candidate_solo_limits]\n3 = -51\n", wantErr: `candidate_solo_limits."3" = -51: offset must be within [-50, 0]`},
		{name: "cycle replaced", content: "[checking]\ncycle = [\"R7\"]\n", want: cycle},
		{name: "shortest all-core duration", content: "[durations]\nchecking_all_core_s = 4\n", want: allCore},
		{name: "longest short trial", content: "[durations]\nshort_trial_s = 86400\n", want: shortTrial},
		{name: "zero short trial", content: "[durations]\nshort_trial_s = 0\n", wantErr: "durations.short_trial_s = 0: must be within [1, 86400]"},
		{name: "short trial beyond a day", content: "[durations]\nshort_trial_s = 86401\n", wantErr: "durations.short_trial_s = 86401: must be within [1, 86400]"},
		{name: "evidence overridden", content: "[evidence]\nmiss = 0.001\nrate = 0.25\n", want: evidence},
		{name: "zero miss", content: "[evidence]\nmiss = 0\n", wantErr: "evidence.miss = 0: must be within (0, 1)"},
		{name: "one miss", content: "[evidence]\nmiss = 1\n", wantErr: "evidence.miss = 1: must be within (0, 1)"},
		{name: "zero rate", content: "[evidence]\nrate = 0\n", wantErr: "evidence.rate = 0: must be within (0, 1)"},
		{name: "one rate", content: "[evidence]\nrate = 1\n", wantErr: "evidence.rate = 1: must be within (0, 1)"},
		{name: "tolerance overridden", content: "[evidence]\nfailure_rate = 0.1\nsignificance = 0.3\n", want: tolerance},
		{name: "zero failure rate", content: "[evidence]\nfailure_rate = 0\n", wantErr: "evidence.failure_rate = 0: must be within (0, 1)"},
		{name: "one failure rate", content: "[evidence]\nfailure_rate = 1\n", wantErr: "evidence.failure_rate = 1: must be within (0, 1)"},
		{name: "nan failure rate", content: "[evidence]\nfailure_rate = nan\n", wantErr: "evidence.failure_rate = NaN: must be within (0, 1)"},
		{name: "infinite failure rate", content: "[evidence]\nfailure_rate = inf\n", wantErr: "evidence.failure_rate = +Inf: must be within (0, 1)"},
		{name: "zero significance", content: "[evidence]\nsignificance = 0\n", wantErr: "evidence.significance = 0: must be within (0, 1)"},
		{name: "one significance", content: "[evidence]\nsignificance = 1\n", wantErr: "evidence.significance = 1: must be within (0, 1)"},
		{name: "nan significance", content: "[evidence]\nsignificance = nan\n", wantErr: "evidence.significance = NaN: must be within (0, 1)"},
		{name: "infinite significance", content: "[evidence]\nsignificance = -inf\n", wantErr: "evidence.significance = -Inf: must be within (0, 1)"},
		{name: "too many trials", content: "[evidence]\nrate = 1e-300\n", wantErr: "evidence: miss 0.05 and rate 1e-300 need more than 1000 trials per step"},
		{name: "removed confirmation key takes precedence", content: "[durations]\nconfirmation_trial_s = 300\nbogus = 1\n", wantErr: "durations.confirmation_trial_s was removed in togi 0.5.0: confirmation no longer exists; delete the key"},
		{name: "all-core duration too short to split", content: "[durations]\nchecking_all_core_s = 3\n", wantErr: "durations.checking_all_core_s = 3: must be within [4, 86400]"},
		{name: "unknown top-level key", content: "bogus = 1\n", wantErr: "unknown keys: bogus"},
		{name: "unknown nested key", content: "[durations]\nsearch_s = 1\n", wantErr: "unknown keys: durations.search_s"},
		{name: "old checking key rejected", content: "[checking]\nlap = [\"R7\"]\n", wantErr: "unknown keys: checking.lap"},
		{name: "old duration key rejected", content: "[durations]\nstart_s = 120\n", wantErr: "unknown keys: durations.start_s"},
		{name: "syntax error", content: "[durations\n", wantErr: "load config"},
		{name: "wrong type", content: "[durations]\nsearch_trial_s = \"90\"\n", wantErr: "search_trial_s"},
		{name: "positive offset", content: "[start_offsets]\n3 = 1\n", wantErr: `start_offsets."3" = 1: offset must be within [-50, 0]`},
		{name: "offset below floor", content: "[start_offsets]\n3 = -51\n", wantErr: `start_offsets."3" = -51: offset must be within [-50, 0]`},
		{name: "core not a number", content: "[start_offsets]\nx = -1\n", wantErr: `start_offsets."x": core must be a non-negative integer`},
		{name: "negative core", content: "[start_offsets]\n-1 = -1\n", wantErr: `start_offsets."-1": core must be a non-negative integer`},
		{name: "core not canonical", content: "[start_offsets]\n03 = -1\n", wantErr: `start_offsets."03": core must be a non-negative integer`},
		{name: "zero duration", content: "[durations]\nchecking_idle_s = 0\n", wantErr: "durations.checking_idle_s = 0: must be within [1, 86400]"},
		{name: "empty cycle", content: "[checking]\ncycle = []\n", wantErr: "checking.cycle: must not be empty"},
		{name: "cycle not a regime", content: "[checking]\ncycle = [\"R1\", \"R8\"]\n", wantErr: `checking.cycle[1] = "R8": not a regime`},
		{name: "zero threshold", content: "[dead_ends]\nstray_crashes_in_a_row = 0\n", wantErr: "dead_ends.stray_crashes_in_a_row = 0: must be within [1, 100]"},
		{name: "relative backend", content: "[backends]\nmprime = \"bin/mprime\"\n", wantErr: `backends.mprime = "bin/mprime": must be an absolute path`},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			path := filepath.Join(t.TempDir(), "config.toml")
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
	_, err := Load(filepath.Join(t.TempDir(), "missing.toml"))
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
	want := []machine.Regime{machine.R7, machine.R7, machine.R7, machine.R2, machine.R2, machine.R2, machine.R6, machine.R5, machine.R1, machine.R1, machine.R1, machine.R3, machine.R4, machine.R6}
	if diff := cmp.Diff(want, Default().Checking.Cycle); diff != "" {
		t.Fatalf("default cycle mismatch (-want +got):\n%s", diff)
	}
}
