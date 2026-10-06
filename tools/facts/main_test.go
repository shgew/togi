package main

import (
	"bytes"
	"os"
	"path/filepath"
	"testing"

	"github.com/shgew/togi/tools/trialfacts"
)

func TestGeneratePreservesOutputOnSourceError(t *testing.T) {
	for _, tc := range []struct {
		name, text string
	}{
		{"invalid-journal", "{invalid}\n"},
		{"empty-source", ""},
		{"no-facts", `{"seq":1,"kind":"session.start","session":"20260101T000000Z","schema":1,"ruleset":6,"cores":[{"core":0},{"core":1}]}
`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			if tc.text != "" {
				if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte(tc.text), 0600); err != nil {
					t.Fatal(err)
				}
			}
			path := filepath.Join(t.TempDir(), "facts.jsonl.gz")
			want := []byte("committed extract")
			if err := os.WriteFile(path, want, 0600); err != nil {
				t.Fatal(err)
			}
			if _, err := generate(dir, path); err == nil {
				t.Fatal("generation accepted an invalid or empty source")
			}
			got, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			if !bytes.Equal(got, want) {
				t.Fatalf("committed output changed to %q", got)
			}
		})
	}
}

func TestGenerateReplacesOutputAfterSuccess(t *testing.T) {
	dir := t.TempDir()
	text := `{"seq":1,"kind":"session.start","session":"20260101T000000Z","schema":1,"ruleset":6,"cores":[{"core":0},{"core":1}]}
{"seq":2,"kind":"trial.intent","trial":"1","core":0,"offset":-10,"regime":"R1","workload":"fixture","duration_s":90,"condition":"isolated","phase":"search"}
{"seq":3,"kind":"trial.end","trial":"1","outcome":"pass","duration_s":90}
`
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte(text), 0600); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(t.TempDir(), "facts.jsonl.gz")
	if err := os.WriteFile(path, []byte("committed extract"), 0600); err != nil {
		t.Fatal(err)
	}
	n, err := generate(dir, path)
	if err != nil {
		t.Fatal(err)
	}
	if n != 1 {
		t.Fatalf("generate counted %d facts, want 1", n)
	}
	records, err := trialfacts.Read(path)
	if err != nil {
		t.Fatal(err)
	}
	if len(records) != 1 || records[0].Trial != "1" || len(records[0].Profile) != 2 || records[0].Profile[0] != -10 {
		t.Fatalf("replacement extract = %+v", records)
	}
}
