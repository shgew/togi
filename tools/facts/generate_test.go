package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestGenerateReportsDestinationFailuresAndRemovesTemporaryFiles(t *testing.T) {
	dir := t.TempDir()
	text := `{"seq":1,"kind":"session.start","session":"20260101T000000Z","schema":1,"ruleset":6,"cores":[{"core":0}]}
{"seq":2,"kind":"trial.intent","trial":"1","core":0,"offset":-10,"regime":"R1","workload":"fixture","duration_s":90,"condition":"isolated","phase":"search"}
{"seq":3,"kind":"trial.end","trial":"1","outcome":"pass","duration_s":90}
`
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte(text), 0600); err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct {
		name, operation      string
		destinationDirectory bool
	}{
		{"missing-parent", "create extract", false},
		{"destination-directory", "replace extract", true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			parent := t.TempDir()
			path := filepath.Join(parent, "missing", "facts.gz")
			if tc.destinationDirectory {
				path = filepath.Join(parent, "facts.gz")
				if err := os.Mkdir(path, 0700); err != nil {
					t.Fatal(err)
				}
			}
			if err := generate(dir, path); err == nil || !strings.Contains(err.Error(), tc.operation) {
				t.Fatalf("generate error=%v, want %s", err, tc.operation)
			}
			entries, err := os.ReadDir(parent)
			if err != nil {
				t.Fatal(err)
			}
			for _, entry := range entries {
				if strings.HasPrefix(entry.Name(), ".facts-") {
					t.Fatalf("temporary extract leaked: %s", entry.Name())
				}
			}
			if tc.destinationDirectory {
				info, err := os.Stat(path)
				if err != nil {
					t.Fatal(err)
				}
				if !info.IsDir() {
					t.Fatal("failed replacement modified destination")
				}
			}
		})
	}
}
