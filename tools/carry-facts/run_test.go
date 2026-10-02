package main

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestRunRejectsUnsafeOrUnreadableCopies(t *testing.T) {
	for _, tc := range []struct{ name, text, link, want string }{
		{"missing-journal", "", "", "read live recorded facts"},
		{"invalid-journal", "{invalid}\n", "", "read live recorded facts"},
		{"missing-context", `{"seq":1,"kind":"session.start","session":"fixture","schema":2,"ruleset":7,"cores":[{"core":0}]}` + "\n", "", "no recorded BIOS context"},
		{"archive-link", "", "archive", "must not symlink archive"},
		{"journal-link", "", "events.jsonl", "must not symlink events.jsonl"},
		{"lock-link", "", "lock", "must not symlink lock"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			if tc.text != "" {
				if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte(tc.text), 0600); err != nil {
					t.Fatal(err)
				}
			}
			if tc.link != "" {
				if err := os.Symlink(t.TempDir(), filepath.Join(dir, tc.link)); err != nil {
					t.Fatal(err)
				}
			}
			var out, diagnostics bytes.Buffer
			err := run([]string{"--state-dir", dir}, &out, &diagnostics)
			if err == nil || !strings.Contains(err.Error(), tc.want) || out.Len() != 0 {
				t.Fatalf("run error=%v output=%q; want %s", err, out.String(), tc.want)
			}
			if tc.text != "" {
				got, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
				if err != nil {
					t.Fatal(err)
				}
				if diff := cmp.Diff(tc.text, string(got)); diff != "" {
					t.Fatalf("refused source (-want +got):\n%s", diff)
				}
			}
		})
	}
	for _, args := range [][]string{nil, {"--state-dir", t.TempDir(), "extra"}, {"--state-dir", filepath.Join(t.TempDir(), "missing")}, {"--state-dir", os.TempDir()}} {
		var out, diagnostics bytes.Buffer
		if err := run(args, &out, &diagnostics); err == nil || out.Len() != 0 {
			t.Fatalf("accepted unsafe args=%v output=%q error=%v", args, out.String(), err)
		}
	}
}

func TestRunPreparesRecordedFacts(t *testing.T) {
	dir := t.TempDir()
	text := `{"seq":1,"kind":"session.start","session":"20260101T000000Z","schema":2,"ruleset":7,"evidence":1,"cores":[{"core":0}]}
{"seq":2,"kind":"session.context","bios_version":"fixture"}
{"seq":3,"kind":"trial.intent","trial":"1","core":0,"offset":-10,"profile":[-10],"regime":"R1","workload":"fixture","duration_s":90,"condition":"isolated","phase":"search"}
{"seq":4,"kind":"trial.end","trial":"1","outcome":"pass","duration_s":90}
{"seq":5,"kind":"trial.intent","trial":"2","core":0,"offset":-15,"profile":[-15],"regime":"R1","workload":"fixture","duration_s":90,"condition":"isolated","phase":"search"}
{"seq":6,"kind":"trial.end","trial":"2","outcome":"failure","signal":"computation_error","duration_s":30}
`
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte(text), 0600); err != nil {
		t.Fatal(err)
	}
	var out, diagnostics bytes.Buffer
	if err := run([]string{"--state-dir", dir}, &out, &diagnostics); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out.String(), "20260101T000000Z      1        1\n") {
		t.Fatalf("prepared source counts: %q", out.String())
	}
	got, err := os.ReadFile(filepath.Join(dir, "archive", "20260101T000000Z.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(text, string(got)); diff != "" {
		t.Fatalf("archived source (-want +got):\n%s", diff)
	}
}
