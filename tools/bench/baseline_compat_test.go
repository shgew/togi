package main

import (
	"os"
	"path/filepath"
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
