package main

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
)

func TestEvents(t *testing.T) {
	t.Setenv("JOURNAL_STREAM", "")
	time.Local = time.UTC
	fixture, err := os.ReadFile("testdata/events.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	tests := []struct {
		name string
		args []string
	}{
		{"no filters", nil},
		{"core", []string{"--core", "7"}},
		{"kind group", []string{"--kind", "trial"}},
		{"exact kinds", []string{"--kind", "trial.end,tuner.decision"}},
		{"kind without group", []string{"--kind", "failure"}},
		{"trial", []string{"--trial", "0413"}},
		{"time window", []string{"--since", "2026-10-02T01:14:07.15Z", "--until", "2026-10-02T01:15:37.461Z"}},
		{"json", []string{"--json", "--core", "3"}},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dir := t.TempDir()
			if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), fixture, 0o644); err != nil {
				t.Fatal(err)
			}
			var stdout, stderr bytes.Buffer
			args := append([]string{"events", "--state-dir", dir}, tt.args...)
			if code := cli(args, &stdout, &stderr); code != exitOK {
				t.Fatalf("exit %d, stderr %q", code, stderr.String())
			}
			golden(t, "events-"+strings.ReplaceAll(tt.name, " ", "-"), stdout.String())
		})
	}
}

func journalStreamFor(t *testing.T, f *os.File) string {
	t.Helper()
	info, err := f.Stat()
	if err != nil {
		t.Fatal(err)
	}
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok {
		t.Fatalf("stat %s has no device and inode", f.Name())
	}
	return fmt.Sprintf("%d:%d", stat.Dev, stat.Ino)
}

func TestEventsJSONUncoloredInSystemJournal(t *testing.T) {
	t.Setenv("NO_COLOR", "")
	fixture, err := os.ReadFile("testdata/events.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), fixture, 0o644); err != nil {
		t.Fatal(err)
	}
	stdout, err := os.CreateTemp(t.TempDir(), "events-output")
	if err != nil {
		t.Fatal(err)
	}
	defer stdout.Close()
	t.Setenv("JOURNAL_STREAM", journalStreamFor(t, stdout))
	var stderr bytes.Buffer
	args := []string{"events", "--state-dir", dir, "--kind", "failure"}
	if code := cli(args, stdout, &stderr); code != exitOK {
		t.Fatalf("human-readable exit %d: %s", code, stderr.String())
	}
	human, err := os.ReadFile(stdout.Name())
	if err != nil {
		t.Fatal(err)
	}
	if got := string(human); !strings.HasPrefix(got, "<3>\x1b[31m") || !strings.HasSuffix(got, "\x1b[0m\n") {
		t.Fatalf("human-readable failure = %q", got)
	}
	if err := stdout.Truncate(0); err != nil {
		t.Fatal(err)
	}
	if _, err := stdout.Seek(0, 0); err != nil {
		t.Fatal(err)
	}
	stderr.Reset()
	if code := cli(append(args, "--json"), stdout, &stderr); code != exitOK {
		t.Fatalf("JSON exit %d: %s", code, stderr.String())
	}
	jsonOutput, err := os.ReadFile(stdout.Name())
	if err != nil {
		t.Fatal(err)
	}
	want := strings.SplitAfter(string(fixture), "\n")[10]
	if got := string(jsonOutput); got != want || strings.ContainsRune(got, '\x1b') || strings.HasPrefix(got, "<3>") {
		t.Fatalf("JSON = %q, want undecorated %q", got, want)
	}
}

func TestEventsErrors(t *testing.T) {
	dir := t.TempDir()
	var stdout, stderr bytes.Buffer
	if code := cli([]string{"--state-dir", dir, "events"}, &stdout, &stderr); code != exitError {
		t.Fatalf("missing journal: exit %d", code)
	}
	if want := "no journal at " + filepath.Join(dir, "events.jsonl"); !strings.Contains(stderr.String(), want) {
		t.Fatalf("stderr %q, want %q", stderr.String(), want)
	}
	for _, args := range [][]string{{"events", "--core", "x"}, {"events", "--since", "yesterday"}, {"events", "extra"}} {
		if code := cli(args, &stdout, &stderr); code != exitUsage {
			t.Fatalf("%v: exit %d, want %d", args, code, exitUsage)
		}
	}
}

func TestEventsKindValidation(t *testing.T) {
	fixture, err := os.ReadFile("testdata/events.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	for _, args := range [][]string{
		{"--kind", "trial.unknown"},
		{"--kind", "trila"},
		{"--kind", ""},
		{"--kind", "  "},
		{"--kind", ",, "},
		{"--kind", "trial,trila"},
		{"--kind", "trial.end,trial.unknown"},
		{"--kind", "trial", "--kind", ""},
	} {
		t.Run(strings.Join(args, "/"), func(t *testing.T) {
			for _, exists := range []bool{false, true} {
				t.Run(fmt.Sprintf("journal=%t", exists), func(t *testing.T) {
					dir := t.TempDir()
					if exists {
						if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), fixture, 0o644); err != nil {
							t.Fatal(err)
						}
					}
					var stdout, stderr bytes.Buffer
					code := cli(append([]string{"events", "--state-dir", dir}, args...), &stdout, &stderr)
					if code != exitUsage {
						t.Fatalf("exit %d, want %d; stderr %q", code, exitUsage, stderr.String())
					}
					if stdout.Len() != 0 {
						t.Fatalf("usage error printed stdout %q", stdout.String())
					}
					if !strings.Contains(stderr.String(), "valid names:") || !strings.Contains(stderr.String(), "trial.end") || !strings.Contains(stderr.String(), "trial,") {
						t.Fatalf("missing valid exact kinds and groups in stderr %q", stderr.String())
					}
					if strings.Contains(stderr.String(), "no journal") {
						t.Fatalf("journal read took precedence over usage error: %q", stderr.String())
					}
				})
			}
		})
	}
}

func TestEventsKindQueries(t *testing.T) {
	fixture, err := os.ReadFile("testdata/events.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), fixture, 0o644); err != nil {
		t.Fatal(err)
	}
	lines := strings.SplitAfter(string(fixture), "\n")
	for _, tt := range []struct {
		name string
		args []string
		want string
	}{
		{"trimmed mixed kinds", []string{"--kind", " trial.end , failure, "}, lines[8] + lines[10]},
		{"repeated kinds", []string{"--kind", "trial.end", "--kind", "failure"}, lines[8] + lines[10]},
		{"valid without matches", []string{"--kind", "mark.joint"}, ""},
		{"combined filters without matches", []string{"--kind", "trial", "--trial", "absent"}, ""},
	} {
		t.Run(tt.name, func(t *testing.T) {
			var stdout, stderr bytes.Buffer
			code := cli(append([]string{"events", "--state-dir", dir, "--json"}, tt.args...), &stdout, &stderr)
			if code != exitOK {
				t.Fatalf("exit %d; stderr %q", code, stderr.String())
			}
			if diff := cmp.Diff(tt.want, stdout.String()); diff != "" {
				t.Fatalf("events (-want +got):\n%s", diff)
			}
		})
	}
}

func TestEventsUnfilteredUnknownKind(t *testing.T) {
	fixture, err := os.ReadFile("testdata/events.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	future := "{\"seq\":12,\"time\":\"2026-10-02T01:21:00Z\",\"boot\":\"e8f9a0b1-1c2d-4e5f-8a9b-0c1d2e3f4a5b\",\"kind\":\"future.observation\",\"msg\":\"opaque future event\",\"value\":17}\n"
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), append(fixture, future...), 0o644); err != nil {
		t.Fatal(err)
	}
	var stdout, stderr bytes.Buffer
	code := cli([]string{"events", "--state-dir", dir, "--json"}, &stdout, &stderr)
	if code != exitOK {
		t.Fatalf("exit %d; stderr %q", code, stderr.String())
	}
	if diff := cmp.Diff(string(fixture)+future, stdout.String()); diff != "" {
		t.Fatalf("unfiltered events (-want +got):\n%s", diff)
	}
}

func TestReadCommandsReportTornTail(t *testing.T) {
	t.Parallel()
	fixture, err := os.ReadFile("testdata/events.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	for _, command := range []string{"status", "events"} {
		t.Run(command, func(t *testing.T) {
			dir := t.TempDir()
			if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), append(bytes.Clone(fixture), []byte("{torn")...), 0o600); err != nil {
				t.Fatal(err)
			}
			var out, diagnostics bytes.Buffer
			code := testCLI(t, []string{"--state-dir", dir, command}, &out, &diagnostics)
			if code != exitOK || out.Len() == 0 {
				t.Fatalf("exit %d, stdout %q, stderr %q", code, out.String(), diagnostics.String())
			}
			if !strings.Contains(diagnostics.String(), "journal ends with 5 torn bytes; the next run records journal.torn\n") {
				t.Fatalf("missing torn-tail warning: %q", diagnostics.String())
			}
			want := append(bytes.Clone(fixture), []byte("{torn")...)
			got, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(want, got); diff != "" {
				t.Fatalf("read modified torn tail: %s", diff)
			}
		})
	}
}
