package main

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"time"
)

func TestHumanJournalBoundaries(t *testing.T) {
	t.Parallel()
	header := fmt.Sprintf(`{"seq":1,"time":"2026-10-02T01:10:00Z","kind":"session.start","schema":%d,"ruleset":3,"session":"session","cores":[]}`, journal.Schema) + "\n"
	context := `{"seq":2,"time":"2026-10-02T01:10:01Z","kind":"session.context","msg":"context","bios_version":"bios\u202e","board":"board\u001b[2J","cpu_model":"cpu\nforged","microcode":"code\u2066"}` + "\n"
	dir := t.TempDir()
	raw := header + context
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte(raw), 0o644); err != nil {
		t.Fatal(err)
	}
	for _, command := range []string{"status"} {
		t.Run(command, func(t *testing.T) {
			var stdout, stderr bytes.Buffer
			if code := cli([]string{"--state-dir", dir, command}, &stdout, &stderr); code != exitOK {
				t.Fatalf("exit %d: %s", code, stderr.String())
			}
			for _, escaped := range []string{`bios\u202e`, `board\x1b[2J`, `cpu\nforged`, `code\u2066`} {
				if !strings.Contains(stdout.String(), escaped) {
					t.Errorf("missing escaped BIOS field %q in %q", escaped, stdout.String())
				}
			}
			if strings.ContainsAny(stdout.String(), "\x1b\u202e\u2066") || strings.Contains(stdout.String(), "cpu\nforged") {
				t.Fatalf("raw controls in %q", stdout.String())
			}
		})
	}
	var stdout, stderr bytes.Buffer
	if code := cli([]string{"--state-dir", dir, "events", "--json"}, &stdout, &stderr); code != exitOK {
		t.Fatalf("JSON exit %d: %s", code, stderr.String())
	}
	if d := cmp.Diff(raw, stdout.String()); d != "" {
		t.Errorf("raw JSON changed (-want +got): %s", d)
	}
}

func TestHumanJournalReadErrorEscapesUnknownKind(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	raw := `{"seq":1,"time":"2026-10-02T01:10:00Z","kind":"future.\u001b[2J\u202e\nforged"}` + "\n"
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte(raw), 0o644); err != nil {
		t.Fatal(err)
	}
	for _, command := range []string{"events", "status", "reset"} {
		t.Run(command, func(t *testing.T) {
			var stdout, stderr bytes.Buffer
			args := []string{"--state-dir", dir, command}
			if command == "reset" {
				args = append(args, "--all")
			}
			g := globals{stateDir: dir, hostLockPath: filepath.Join(t.TempDir(), "togi.lock")}
			if code := cliWithGlobals(args, &stdout, &stderr, g); code != exitError {
				t.Fatalf("exit %d: %s", code, stderr.String())
			}
			if !strings.Contains(stderr.String(), `future.\x1b[2J\u202e\nforged`) {
				t.Errorf("unknown kind not escaped: %q", stderr.String())
			}
			if strings.ContainsAny(stderr.String(), "\x1b\u202e") || strings.Contains(stderr.String(), "\nforged") {
				t.Errorf("raw controls: %q", stderr.String())
			}
		})
	}
}

func TestHumanProjectedFieldsEscapeControls(t *testing.T) {
	t.Parallel()
	text := "日本語\u202e\x1b[2J\nforged"
	escaped := `日本語\u202e\x1b[2J\nforged`
	st := journal.State{
		Session:   &journal.SessionInfo{ID: text, Start: time.Date(2026, 10, 2, 1, 0, 0, 0, time.UTC)},
		Cores:     []journal.CoreState{{Core: 0, Phase: journal.Phase(text), Queued: text}},
		Hunt:      &journal.HuntState{Trial: text, Regime: machine.Regime(text), Groups: []journal.GroupState{{Outcome: text}}},
		Deepening: &journal.DeepeningState{Checks: []journal.CheckState{{Regime: machine.Regime(text), Workload: text}}},
		Checking:  &journal.CheckingState{Exposure: []journal.ExposureRow{{Regime: machine.Regime(text), Workload: text}}},
	}
	for _, render := range []struct {
		name  string
		write func(*bytes.Buffer)
	}{
		{"status", func(out *bytes.Buffer) { writeStatus(out, st, nil) }},
	} {
		t.Run(render.name, func(t *testing.T) {
			var out bytes.Buffer
			render.write(&out)
			if !strings.Contains(out.String(), escaped) || strings.ContainsAny(out.String(), "\x1b\u202e") || strings.Contains(out.String(), "\nforged") {
				t.Fatalf("projected controls not escaped: %q", out.String())
			}
		})
	}
}
