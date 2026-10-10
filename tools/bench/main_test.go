package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/tools/trialfacts"
)

func TestLoadRunsRejectsInvalidSuite(t *testing.T) {
	for _, tc := range []struct {
		name, suite, want string
	}{
		{"syntax", "[", "load suite:"},
		{"unknown key", "{\"extra\":true}", "unknown object member name"},
		{"empty name", "{\"scenarios\":[{\"dev\":[1]}]}", "invalid or duplicate scenario"},
		{"duplicate name", "{\"scenarios\":[{\"name\":\"x\",\"dev\":[1]},{\"name\":\"x\",\"dev\":[2]}]}", "invalid or duplicate scenario"},
		{"nested name", "{\"scenarios\":[{\"name\":\"x/y\",\"dev\":[1]}]}", "invalid or duplicate scenario"},
		{"dot name", "{\"scenarios\":[{\"name\":\".\",\"dev\":[1]}]}", "invalid or duplicate scenario"},
		{"parent name", "{\"scenarios\":[{\"name\":\"..\",\"dev\":[1]}]}", "invalid or duplicate scenario"},
		{"conflicting machines", "{\"scenarios\":[{\"name\":\"x\",\"machine\":\"a\",\"machines\":[\"b\"],\"dev\":[1]}]}", "load scenario x: machine and machines are mutually exclusive"},
		{"missing machine", "{\"scenarios\":[{\"name\":\"x\",\"machine\":\"missing.json\",\"dev\":[1]}]}", "load scenario x:"},
		{"replay without facts", "{\"scenarios\":[{\"name\":\"x\",\"replay\":true,\"dev\":[1]}]}", "load scenario x:"},
		{"repeated selected seed", "{\"scenarios\":[{\"name\":\"x\",\"dev\":[1,1]}]}", "scenario x repeats seed 1"},
		{"repeated unselected seed", "{\"scenarios\":[{\"name\":\"x\",\"dev\":[1],\"holdout\":[2,2]}]}", "scenario x repeats seed 2"},
		{"overlapping splits", "{\"scenarios\":[{\"name\":\"x\",\"dev\":[1],\"holdout\":[1]}]}", "scenario x repeats seed 1"},
		{"smoke seed outside both splits", "{\"scenarios\":[{\"name\":\"x\",\"dev\":[1],\"smoke\":[2]}]}", "scenario x smoke seed 2 is in neither dev nor holdout"},
		{"no selected runs", "{\"scenarios\":[{\"name\":\"x\",\"holdout\":[1]}]}", "suite has no selected runs"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "suite.json")
			if err := os.WriteFile(path, []byte(tc.suite), 0600); err != nil {
				t.Fatal(err)
			}
			_, err := loadRuns(path, "dev", trialfacts.Extracts{})
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("loadRuns error = %v, want %q", err, tc.want)
			}
		})
	}
	t.Run("missing suite", func(t *testing.T) {
		_, err := loadRuns(filepath.Join(t.TempDir(), "missing.json"), "dev", trialfacts.Extracts{})
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

func TestLoadRunsMarksSmokeSessions(t *testing.T) {
	path := filepath.Join(t.TempDir(), "suite.json")
	suite := "{\"scenarios\":[{\"name\":\"x\",\"dev\":[1,2],\"holdout\":[3],\"smoke\":[2,3]},{\"name\":\"y\",\"dev\":[1]}]}"
	if err := os.WriteFile(path, []byte(suite), 0600); err != nil {
		t.Fatal(err)
	}
	runs, err := loadRuns(path, "all", trialfacts.Extracts{})
	if err != nil {
		t.Fatal(err)
	}
	var got []string
	for _, run := range runs {
		if run.smoke {
			got = append(got, sessionKey{run.scenario.Name, run.split, run.seed}.String())
		}
	}
	if diff := cmp.Diff([]string{"x/dev-2", "x/holdout-3"}, got); diff != "" {
		t.Fatal(diff)
	}
}
