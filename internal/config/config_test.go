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
	start := Default()
	start.Durations.StartS = 86400
	evidence := Default()
	evidence.Evidence = Evidence{Miss: 0.001, Rate: 0.25}
	offsets := Default()
	offsets.StartOffsets = map[int]int{3: -10}
	edges := Default()
	edges.StartOffsets = map[int]int{2: -5}
	edges.CandidateEdges = map[int]int{3: -36}
	rotation := Default()
	allCore := Default()
	allCore.Durations.GuardAllCoreS = 4
	rotation.Guard.Rotation = []machine.Regime{machine.R7}
	user := Default()
	user.BackendUser = "togi-trial"

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
		{name: "candidate edge beside another core's start offset", content: "[start_offsets]\n2 = -5\n[candidate_edges]\n3 = -36\n", want: edges},
		{name: "candidate edge and start offset for one core", content: "[start_offsets]\n3 = -30\n[candidate_edges]\n3 = -36\n", wantErr: `start_offsets."3" and candidate_edges."3": set at most one per core`},
		{name: "candidate edge below floor", content: "[candidate_edges]\n3 = -51\n", wantErr: `candidate_edges."3" = -51: offset must be within [-50, 0]`},
		{name: "rotation replaced", content: "[guard]\nrotation = [\"R7\"]\n", want: rotation},
		{name: "shortest all-core duration", content: "[durations]\nguard_all_core_s = 4\n", want: allCore},
		{name: "longest start", content: "[durations]\nstart_s = 86400\n", want: start},
		{name: "zero start", content: "[durations]\nstart_s = 0\n", wantErr: "durations.start_s = 0: must be within [1, 86400]"},
		{name: "start beyond a day", content: "[durations]\nstart_s = 86401\n", wantErr: "durations.start_s = 86401: must be within [1, 86400]"},
		{name: "evidence overridden", content: "[evidence]\nmiss = 0.001\nrate = 0.25\n", want: evidence},
		{name: "zero miss", content: "[evidence]\nmiss = 0\n", wantErr: "evidence.miss = 0: must be within (0, 1)"},
		{name: "one miss", content: "[evidence]\nmiss = 1\n", wantErr: "evidence.miss = 1: must be within (0, 1)"},
		{name: "zero rate", content: "[evidence]\nrate = 0\n", wantErr: "evidence.rate = 0: must be within (0, 1)"},
		{name: "one rate", content: "[evidence]\nrate = 1\n", wantErr: "evidence.rate = 1: must be within (0, 1)"},
		{name: "too many starts", content: "[evidence]\nrate = 1e-300\n", wantErr: "evidence: miss 0.05 and rate 1e-300 need more than 1000 starts per step"},
		{name: "removed confirmation key takes precedence", content: "[durations]\nconfirmation_trial_s = 300\nbogus = 1\n", wantErr: "durations.confirmation_trial_s was removed in togi 0.5.0: confirmation no longer exists; delete the key"},
		{name: "all-core duration too short to split", content: "[durations]\nguard_all_core_s = 3\n", wantErr: "durations.guard_all_core_s = 3: must be within [4, 86400]"},
		{name: "unknown top-level key", content: "bogus = 1\n", wantErr: "unknown keys: bogus"},
		{name: "unknown nested key", content: "[durations]\nsearch_s = 1\n", wantErr: "unknown keys: durations.search_s"},
		{name: "syntax error", content: "[durations\n", wantErr: "load config"},
		{name: "wrong type", content: "[durations]\nsearch_trial_s = \"90\"\n", wantErr: "search_trial_s"},
		{name: "positive offset", content: "[start_offsets]\n3 = 1\n", wantErr: `start_offsets."3" = 1: offset must be within [-50, 0]`},
		{name: "offset below floor", content: "[start_offsets]\n3 = -51\n", wantErr: `start_offsets."3" = -51: offset must be within [-50, 0]`},
		{name: "core not a number", content: "[start_offsets]\nx = -1\n", wantErr: `start_offsets."x": core must be a non-negative integer`},
		{name: "negative core", content: "[start_offsets]\n-1 = -1\n", wantErr: `start_offsets."-1": core must be a non-negative integer`},
		{name: "core not canonical", content: "[start_offsets]\n03 = -1\n", wantErr: `start_offsets."03": core must be a non-negative integer`},
		{name: "zero duration", content: "[durations]\nguard_idle_s = 0\n", wantErr: "durations.guard_idle_s = 0: must be within [1, 86400]"},
		{name: "empty rotation", content: "[guard]\nrotation = []\n", wantErr: "guard.rotation: must not be empty"},
		{name: "rotation not a regime", content: "[guard]\nrotation = [\"R1\", \"R8\"]\n", wantErr: `guard.rotation[1] = "R8": not a regime`},
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

func TestEvidenceStarts(t *testing.T) {
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
			if diff := cmp.Diff(tt.want, tt.evidence.Starts()); diff != "" {
				t.Fatalf("Starts mismatch (-want +got):\n%s", diff)
			}
		})
	}
}

func TestDefaultRotation(t *testing.T) {
	t.Parallel()
	want := []machine.Regime{machine.R7, machine.R7, machine.R7, machine.R2, machine.R2, machine.R2, machine.R6, machine.R5, machine.R1, machine.R1, machine.R1, machine.R3, machine.R4, machine.R6}
	if diff := cmp.Diff(want, Default().Guard.Rotation); diff != "" {
		t.Fatalf("default rotation mismatch (-want +got):\n%s", diff)
	}
}
