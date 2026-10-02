package main

import (
	"bytes"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/tuner"
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
	temp := t.TempDir()
	t.Setenv("TMPDIR", temp)
	dir := filepath.Join(temp, "copy")
	if err := os.Mkdir(dir, 0700); err != nil {
		t.Fatal(err)
	}
	for _, path := range []string{temp, dir} {
		if err := os.WriteFile(filepath.Join(path, "events.jsonl"), []byte(recordedFacts("20260101T000000Z", "pass")), 0600); err != nil {
			t.Fatal(err)
		}
	}
	for _, tc := range []struct {
		args []string
		want string
	}{
		{nil, "supply --state-dir with a temporary state copy and no positional arguments"},
		{[]string{"--state-dir", dir, "extra"}, "supply --state-dir with a temporary state copy and no positional arguments"},
		{[]string{"--state-dir", filepath.Join(temp, "missing")}, "resolve state copy"},
		{[]string{"--state-dir", temp}, "--state-dir must name a copy beneath"},
	} {
		var out, diagnostics bytes.Buffer
		if err := run(tc.args, &out, &diagnostics); err == nil || !strings.Contains(err.Error(), tc.want) || out.Len() != 0 {
			t.Fatalf("unsafe args=%v output=%q error=%v; want %s", tc.args, out.String(), err, tc.want)
		}
	}
}

func TestRunPreparesRecordedFacts(t *testing.T) {
	dir := t.TempDir()
	text := recordedFacts("20260102T000000Z", "pass")
	older := recordedFacts("20260101T000000Z", "failure")
	if err := os.Mkdir(filepath.Join(dir, "archive"), 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "archive", "20260101T000000Z.jsonl"), []byte(older), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte(text), 0600); err != nil {
		t.Fatal(err)
	}
	var out, diagnostics bytes.Buffer
	if err := run([]string{"--state-dir", dir}, &out, &diagnostics); err != nil {
		t.Fatal(err)
	}
	want := fmt.Sprintf("evidence epoch %d; prepared ruleset %d transition in temporary copy\nSOURCE SESSION       PASSES FAILURES\n20260101T000000Z      1        2\n20260102T000000Z      2        1\n", tuner.EvidenceEpoch, tuner.Ruleset+1)
	if diff := cmp.Diff(want, out.String()); diff != "" {
		t.Fatalf("prepared source report (-want +got):\n%s", diff)
	}
	got, err := os.ReadFile(filepath.Join(dir, "archive", "20260102T000000Z.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(text, string(got)); diff != "" {
		t.Fatalf("archived source (-want +got):\n%s", diff)
	}
}

func recordedFacts(id, lastOutcome string) string {
	signal := ""
	if lastOutcome == "failure" {
		signal = `,"signal":"computation_error"`
	}
	return fmt.Sprintf(`{"seq":1,"kind":"session.start","session":%q,"schema":2,"ruleset":7,"evidence":1,"cores":[{"core":0}]}
{"seq":2,"kind":"session.context","bios_version":"fixture"}
{"seq":3,"kind":"trial.intent","trial":"1","core":0,"offset":-10,"profile":[-10],"regime":"R1","workload":"fixture","duration_s":90,"condition":"isolated","phase":"search"}
{"seq":4,"kind":"trial.end","trial":"1","outcome":"pass","duration_s":90}
{"seq":5,"kind":"trial.intent","trial":"2","core":0,"offset":-15,"profile":[-15],"regime":"R1","workload":"fixture","duration_s":90,"condition":"isolated","phase":"search"}
{"seq":6,"kind":"trial.end","trial":"2","outcome":"failure","signal":"computation_error","duration_s":30}
{"seq":7,"kind":"trial.intent","trial":"3","core":0,"offset":-12,"profile":[-12],"regime":"R1","workload":"fixture","duration_s":90,"condition":"isolated","phase":"search"}
{"seq":8,"kind":"trial.end","trial":"3","outcome":%q%s,"duration_s":90}
`, id, lastOutcome, signal)
}

func TestRunRefusesLockedOrBrokenPreparation(t *testing.T) {
	for _, locked := range []bool{false, true} {
		t.Run(map[bool]string{false: "broken archive", true: "locked"}[locked], func(t *testing.T) {
			dir := t.TempDir()
			text := recordedFacts("20260101T000000Z", "pass")
			if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte(text), 0600); err != nil {
				t.Fatal(err)
			}
			want := "prepare transition"
			if locked {
				j, err := journal.Lock(dir, journal.Options{})
				if err != nil {
					t.Fatal(err)
				}
				defer j.Close()
				want = "lock state copy"
			} else if err := os.WriteFile(filepath.Join(dir, "archive"), nil, 0600); err != nil {
				t.Fatal(err)
			}
			var out, diagnostics bytes.Buffer
			err := run([]string{"--state-dir", dir}, &out, &diagnostics)
			if err == nil || !strings.Contains(err.Error(), want) || out.Len() != 0 {
				t.Fatalf("refused preparation: error=%v output=%q; want %s", err, out.String(), want)
			}
			if locked && !errors.Is(err, journal.ErrLocked) {
				t.Fatalf("lock error identity lost: %v", err)
			}
			got, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(text, string(got)); diff != "" {
				t.Fatalf("refused preparation changed source (-want +got):\n%s", diff)
			}
		})
	}
}
