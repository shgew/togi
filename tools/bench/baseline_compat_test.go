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

func TestReadResultsIgnoresRetiredFields(t *testing.T) {
	path := filepath.Join(t.TempDir(), "baseline.jsonl")
	line := `{"scenario":"default","seed":1,"split":"dev","status":"concluded","sim_hours":10,"passed_cycles":2,"partial_seconds":150}` + "\n"
	if err := os.WriteFile(path, []byte(line), 0o600); err != nil {
		t.Fatal(err)
	}
	got, err := readResults(path)
	if err != nil {
		t.Fatal(err)
	}
	want := []result{{Scenario: "default", Seed: 1, Split: "dev", Status: "concluded", SimHours: 10, PassedCycles: 2}}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("baseline with a retired field (-want +got):\n%s", diff)
	}
}

func TestComparisonIgnoresProjectedFields(t *testing.T) {
	dir := t.TempDir()
	full := `{"scenario":"default","seed":1,"split":"dev","status":"concluded","sim_hours":10,"depth":-30,"trials":5,"wall_s":1.5,"partial_seconds":150,"model_check":{"checks":[{"name":"x"}]}}` + "\n"
	slim := `{"scenario":"default","seed":1,"split":"dev","status":"concluded","sim_hours":10,"depth":-30,"trials":5}` + "\n"
	candidate := []result{{Scenario: "default", Seed: 1, Split: "dev", Status: "concluded", SimHours: 12, Depth: -30, Trials: 6}}
	var reports []string
	for name, content := range map[string]string{"full": full, "slim": slim} {
		path := filepath.Join(dir, name+".jsonl")
		if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
			t.Fatal(err)
		}
		baseline, err := readResults(path)
		if err != nil {
			t.Fatal(err)
		}
		var out strings.Builder
		reportComparison(&out, candidate, baseline)
		reports = append(reports, out.String())
	}
	if diff := cmp.Diff(reports[0], reports[1]); diff != "" {
		t.Fatalf("comparison against full and slim baselines (-full +slim):\n%s", diff)
	}
}

func TestCommittedBaselineIsSlim(t *testing.T) {
	data, err := os.ReadFile("baseline.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	for i, line := range bytes.Split(bytes.TrimSpace(data), []byte("\n")) {
		var fields map[string]json.RawMessage
		if err := json.Unmarshal(line, &fields); err != nil {
			t.Fatalf("baseline line %d: %v", i+1, err)
		}
		for _, name := range []string{"model_check", "wall_s", "partial_seconds"} {
			if _, ok := fields[name]; ok {
				t.Errorf("baseline line %d has %s; just bench-baseline leaves it out", i+1, name)
			}
		}
	}
}
