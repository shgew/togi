package main

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/shgew/togi/internal/journal"
)

func TestWatchWithoutJournal(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	var stdout, stderr bytes.Buffer
	if code := cli([]string{"--state-dir", dir, "watch", "--width", "120", "--height", "33"}, &stdout, &stderr); code != exitOK {
		t.Fatalf("exit %d, stderr %q", code, stderr.String())
	}
	if !strings.Contains(stdout.String(), "no session yet") {
		t.Errorf("stdout %q, want it to say no session yet", stdout.String())
	}
	if stderr.Len() != 0 {
		t.Errorf("stderr %q, want empty", stderr.String())
	}
	if code := cli([]string{"--state-dir", dir, "watch", "--width", "0"}, &stdout, &stderr); code != exitUsage {
		t.Errorf("--width 0: exit %d, want %d", code, exitUsage)
	}
}

func TestWatchProblemFrame(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		fixture string
		problem string
	}{
		{name: "malformed", problem: "invalid character"},
		{name: "incompatible-schema", fixture: "schema", problem: "uses schema 2"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			var dir string
			if tc.fixture != "" {
				dir, _ = incompatibleFixture(t, tc.fixture)
			} else {
				dir = t.TempDir()
				if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte("not json\n"), 0o644); err != nil {
					t.Fatal(err)
				}
			}
			var stdout, stderr bytes.Buffer
			code := cli([]string{"--state-dir", dir, "watch", "--width", "120", "--height", "33"}, &stdout, &stderr)
			if code != exitError {
				t.Errorf("exit %d, want %d", code, exitError)
			}
			if !strings.Contains(strings.Join(strings.Fields(stdout.String()), " "), tc.problem) {
				t.Errorf("stdout %q, want problem %q", stdout.String(), tc.problem)
			}
			if !strings.Contains(stderr.String(), "togi watch: ") || !strings.Contains(stderr.String(), tc.problem) {
				t.Errorf("stderr %q, want watch error %q", stderr.String(), tc.problem)
			}
		})
	}
}

func TestWatchEscapesJournalControls(t *testing.T) {
	t.Parallel()
	dir, original := incompatibleFixture(t, "schema")
	data := bytes.ReplaceAll(original, []byte("0.2.1"), []byte(`\u001b[31m0.2.1\u001b[0m\u001b]52;c;data\u0007\r\n\u009b2J\u2028`))
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), data, 0o644); err != nil {
		t.Fatal(err)
	}
	var stdout, stderr bytes.Buffer
	if code := cli([]string{"--state-dir", dir, "watch"}, &stdout, &stderr); code != exitError {
		t.Fatalf("exit %d, want %d", code, exitError)
	}
	for name, text := range map[string]string{"stdout": stdout.String(), "stderr": stderr.String()} {
		if strings.ContainsAny(text, "\x1b\r\u009b\u2028") || !strings.Contains(text, `\x1b[31m0.2.1\x1b[0m\x1b]52;c;data\x07\r\n\u009b2J\u2028+def5678`) {
			t.Errorf("%s %q, want visibly escaped diagnostic controls", name, text)
		}
	}
}

func TestStatusEscapesJournalMessages(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 9, 30, 12, 0, 0, 0, time.UTC)
	msg := "日本語\x1b[2J\x1b]52;c;data\x07\r\n\u009b31m\u2028"
	escaped := `日本語\x1b[2J\x1b]52;c;data\x07\r\n\u009b31m\u2028`
	st := journal.State{
		Session:  &journal.SessionInfo{ID: "diagnostics", Start: now},
		InFlight: &journal.InFlight{Seq: 2, Msg: msg},
		Cores:    []journal.CoreState{{Core: 0, LastDecision: &journal.DecisionRef{Seq: 5, Msg: msg}}},
	}
	var out bytes.Buffer
	writeStatus(&out, st, []journal.Event{
		{Seq: 3, Kind: journal.KindSessionCarried, Msg: msg},
		{Seq: 4, Kind: journal.KindMCE, Msg: msg, Data: &journal.MCE{BetweenTrials: true}},
	})
	for _, prefix := range []string{"in flight: [#2] ", "carried: [#3] ", "between-trial evidence [#4]: ", "[#5] "} {
		if !strings.Contains(out.String(), prefix+escaped) {
			t.Fatalf("status diagnostic %q not visibly escaped: %q", prefix, out.String())
		}
	}
	if strings.ContainsAny(out.String(), "\x1b\r\u009b\u2028") {
		t.Fatalf("status contains executable controls: %q", out.String())
	}
}
