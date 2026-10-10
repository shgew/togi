package main

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestTimingChecks(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		records []runRecord
		want    []finding
	}{
		{"passing", []runRecord{{Scenario: "a", Status: "concluded", WallS: 1}, {Scenario: "a", Status: "deadend", WallS: 10}}, nil},
		{"strict tenfold boundary", []runRecord{{Scenario: "a", Status: "concluded", WallS: 1}, {Scenario: "a", Status: "concluded", WallS: 1}, {Scenario: "a", Status: "concluded", WallS: 10}}, nil},
		{"outlier", []runRecord{{Scenario: "a", Status: "concluded", WallS: 1}, {Scenario: "a", Status: "concluded", WallS: 1}, {Scenario: "a", Status: "concluded", WallS: 11}}, []finding{{0, "wall_time"}}},
		{"scenario medians separate", []runRecord{{Scenario: "a", Status: "concluded", WallS: 1}, {Scenario: "b", Status: "concluded", WallS: 100}}, nil},
		{"execution error", []runRecord{{Scenario: "a", Status: "error", WallS: 1}}, []finding{{0, "termination"}}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if diff := cmp.Diff(tc.want, findings(timingViolations(tc.records, "root"))); diff != "" {
				t.Fatalf("timing (-want +got):\n%s", diff)
			}
		})
	}
}
func TestSweepSuite(t *testing.T) {
	t.Parallel()
	base := t.TempDir()
	input := suite{Gate: map[string]any{"id": "ruleset-11"}, Scenarios: []scenario{{Name: "default", Dev: []uint64{100}, Holdout: []uint64{200}}, {Name: "single", Machine: "one.json"}, {Name: "ensemble", Machines: []string{"", "one.json", filepath.Join(base, "two.json")}, Replay: true}}}
	want := suite{Scenarios: []scenario{{Name: "default", Dev: []uint64{1, 2, 3}}, {Name: "single", Machine: filepath.Join(base, "one.json"), Dev: []uint64{1, 2, 3}}, {Name: "ensemble", Machines: []string{"", filepath.Join(base, "one.json"), filepath.Join(base, "two.json")}, Replay: true, Dev: []uint64{1, 2, 3}}}}
	if diff := cmp.Diff(want, sweepSuite(input, base, 3)); diff != "" {
		t.Fatalf("suite (-want +got):\n%s", diff)
	}
}

// TestSweepSuiteWire drives the production file reader and writer with real JSON and checks the exact member names and
// values tools/bench reads, independent of the audit structs.
func TestSweepSuiteWire(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	abs := filepath.Join(t.TempDir(), "abs.json")
	quote := func(s string) string {
		b, err := json.Marshal(s)
		if err != nil {
			t.Fatal(err)
		}
		return string(b)
	}
	in := `{"scenarios":[
		{"name":"default","dev":[100],"holdout":[200],"smoke":[300]},
		{"name":"relative","machine":"machines/one.json","dev":[7]},
		{"name":"absolute","machine":` + quote(abs) + `,"holdout":[9]},
		{"name":"ensemble","machines":["","machines/two.json",` + quote(abs) + `],"replay":true,"dev":[5],"smoke":[6]}
	]}`
	want := `{"scenarios":[
		{"name":"default","dev":[1,2,3]},
		{"name":"relative","machine":` + quote(filepath.Join(dir, "machines/one.json")) + `,"dev":[1,2,3]},
		{"name":"absolute","machine":` + quote(abs) + `,"dev":[1,2,3]},
		{"name":"ensemble","machines":["",` + quote(filepath.Join(dir, "machines/two.json")) + `,` + quote(abs) + `],"replay":true,"dev":[1,2,3]}
	]}`
	src := filepath.Join(dir, "suite.json")
	if err := os.WriteFile(src, []byte(in), 0o644); err != nil {
		t.Fatal(err)
	}
	loaded, err := loadSweepSuite(src, 3)
	if err != nil {
		t.Fatal(err)
	}
	out := filepath.Join(t.TempDir(), "out.json")
	if err := writeSweepSuite(out, loaded); err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(out)
	if err != nil {
		t.Fatal(err)
	}
	var got, wantAny any
	if err := json.Unmarshal(data, &got); err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal([]byte(want), &wantAny); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(wantAny, got); diff != "" {
		t.Fatalf("emitted suite (-want +got):\n%s", diff)
	}
}

func TestLoadSweepSuiteRejects(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct{ name, in, want string }{
		{"malformed", `{"scenarios":[`, "read suite"},
		{"unknown top-level member", `{"scenarios":[{"name":"a"}],"extra":1}`, "read suite"},
		{"unknown scenario member", `{"scenarios":[{"name":"a","machiness":["x"]}]}`, "read suite"},
		{"wrong type", `{"scenarios":[{"name":"a","replay":"yes"}]}`, "read suite"},
		{"no scenarios", `{"scenarios":[]}`, "suite has no scenarios"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "suite.json")
			if err := os.WriteFile(path, []byte(tc.in), 0o644); err != nil {
				t.Fatal(err)
			}
			_, err := loadSweepSuite(path, 3)
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("error = %v, want containing %q", err, tc.want)
			}
		})
	}
	if _, err := loadSweepSuite(filepath.Join(t.TempDir(), "missing.json"), 3); err == nil || !strings.Contains(err.Error(), "read suite") {
		t.Fatalf("missing file error = %v", err)
	}
}
func TestUsage(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		args []string
		want int
	}{{[]string{"--help"}, 0}, {[]string{"--seeds", "0"}, 2}, {[]string{"--jobs", "0"}, 2}, {[]string{"--timeout", "0s"}, 2}, {[]string{"--records", "file"}, 2}, {[]string{"--root", "dir"}, 2}} {
		var stdout, stderr bytes.Buffer
		if diff := cmp.Diff(tc.want, run(tc.args, &stdout, &stderr)); diff != "" {
			t.Fatalf("%v exit (-want +got):\n%s", tc.args, diff)
		}
	}
}
