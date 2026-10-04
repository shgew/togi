package main

import (
	"bytes"
	"path/filepath"
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
		{"timeout", []runRecord{{Scenario: "a", Status: "timeout", WallS: 180}}, []finding{{0, "wall_time"}}},
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
	input := suite{[]scenario{{Name: "default", Dev: []uint64{100}, Holdout: []uint64{200}}, {Name: "single", Machine: "one.toml"}, {Name: "ensemble", Machines: []string{"", "one.toml", filepath.Join(base, "two.toml")}, Replay: true}}}
	want := suite{[]scenario{{Name: "default", Dev: []uint64{1, 2, 3}}, {Name: "single", Machine: filepath.Join(base, "one.toml"), Dev: []uint64{1, 2, 3}}, {Name: "ensemble", Machines: []string{"", filepath.Join(base, "one.toml"), filepath.Join(base, "two.toml")}, Replay: true, Dev: []uint64{1, 2, 3}}}}
	if diff := cmp.Diff(want, sweepSuite(input, base, 3)); diff != "" {
		t.Fatalf("suite (-want +got):\n%s", diff)
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
