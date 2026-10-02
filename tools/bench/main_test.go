package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestLoadRunsRejectsInvalidSuite(t *testing.T) {
	for _, tc := range []struct {
		name, suite, want string
	}{
		{"syntax", "[", "load suite:"},
		{"unknown key", "extra = true", "unknown suite key extra"},
		{"empty name", "[[scenario]]\ndev = [1]", "invalid or duplicate scenario"},
		{"duplicate name", "[[scenario]]\nname = 'x'\ndev = [1]\n[[scenario]]\nname = 'x'\ndev = [2]", "invalid or duplicate scenario"},
		{"nested name", "[[scenario]]\nname = 'x/y'\ndev = [1]", "invalid or duplicate scenario"},
		{"dot name", "[[scenario]]\nname = '.'\ndev = [1]", "invalid or duplicate scenario"},
		{"parent name", "[[scenario]]\nname = '..'\ndev = [1]", "invalid or duplicate scenario"},
		{"conflicting machines", "[[scenario]]\nname = 'x'\nmachine = 'a'\nmachines = ['b']\ndev = [1]", "load scenario x: machine and machines are mutually exclusive"},
		{"missing machine", "[[scenario]]\nname = 'x'\nmachine = 'missing.toml'\ndev = [1]", "load scenario x:"},
		{"replay without facts", "[[scenario]]\nname = 'x'\nreplay = true\ndev = [1]", "load scenario x:"},
		{"repeated selected seed", "[[scenario]]\nname = 'x'\ndev = [1, 1]", "scenario x repeats seed 1"},
		{"repeated unselected seed", "[[scenario]]\nname = 'x'\ndev = [1]\nholdout = [2, 2]", "scenario x repeats seed 2"},
		{"overlapping splits", "[[scenario]]\nname = 'x'\ndev = [1]\nholdout = [1]", "scenario x repeats seed 1"},
		{"no selected runs", "[[scenario]]\nname = 'x'\nholdout = [1]", "suite has no selected runs"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "suite.toml")
			if err := os.WriteFile(path, []byte(tc.suite), 0600); err != nil {
				t.Fatal(err)
			}
			_, err := loadRuns(path, "dev")
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("loadRuns error = %v, want %q", err, tc.want)
			}
		})
	}
	t.Run("missing suite", func(t *testing.T) {
		_, err := loadRuns(filepath.Join(t.TempDir(), "missing.toml"), "dev")
		if err == nil || !strings.Contains(err.Error(), "load suite:") {
			t.Fatalf("loadRuns error = %v, want load suite diagnostic", err)
		}
	})
}

func TestReadResults(t *testing.T) {
	for _, tc := range []struct {
		name, input, wantErr string
		want                 []result
	}{
		{name: "empty"},
		{name: "whitespace", input: "\n \t\n"},
		{name: "records", input: "{\"scenario\":\"target\",\"seed\":1,\"split\":\"dev\",\"status\":\"concluded\",\"sim_hours\":2,\"depth\":-40,\"real_answers\":3,\"trials\":4}\n{\"scenario\":\"default\",\"seed\":1}\n", want: []result{{Scenario: "target", Seed: 1, Split: "dev", Status: "concluded", SimHours: 2, Depth: -40, RealAnswers: 3, Trials: 4}, {Scenario: "default", Seed: 1}}},
		{name: "malformed", input: "{", wantErr: "decode baseline:"},
		{name: "malformed tail", input: "{\"scenario\":\"target\",\"seed\":1}\n{", wantErr: "decode baseline:"},
		{name: "duplicate across splits", input: "{\"scenario\":\"target\",\"seed\":1,\"split\":\"dev\"}\n{\"scenario\":\"target\",\"seed\":1,\"split\":\"holdout\"}", wantErr: "duplicate baseline run target/1"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "baseline.jsonl")
			if err := os.WriteFile(path, []byte(tc.input), 0600); err != nil {
				t.Fatal(err)
			}
			got, err := readResults(path)
			if tc.wantErr != "" {
				if err == nil || !strings.Contains(err.Error(), tc.wantErr) {
					t.Fatalf("readResults error = %v, want %q", err, tc.wantErr)
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Fatal(diff)
			}
		})
	}
	t.Run("missing baseline", func(t *testing.T) {
		_, err := readResults(filepath.Join(t.TempDir(), "missing.jsonl"))
		if err == nil || !strings.Contains(err.Error(), "open baseline:") {
			t.Fatalf("readResults error = %v, want open baseline diagnostic", err)
		}
	})
}
