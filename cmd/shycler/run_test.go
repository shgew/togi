package main

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/session"
)

func TestRunRejectsSimInTuningBoot(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	grubenv := filepath.Join(dir, "grubenv")
	const env = "# GRUB Environment Block\nsaved_entry=shycler\n"
	if err := os.WriteFile(grubenv, []byte(env), 0o644); err != nil {
		t.Fatal(err)
	}
	state := filepath.Join(dir, "state")
	var stdout, stderr bytes.Buffer
	if code := cli([]string{"--state-dir", state, "run", "--sim", "1", "--tuning-boot", grubenv}, &stdout, &stderr); code != exitUsage {
		t.Fatalf("exit %d, want %d; stderr %s", code, exitUsage, stderr.String())
	}
	if got, err := os.ReadFile(grubenv); err != nil || string(got) != env {
		t.Fatalf("grubenv changed to %q (%v)", got, err)
	}
	if _, err := os.Stat(filepath.Join(state, "events.jsonl")); !os.IsNotExist(err) {
		t.Fatalf("simulated session started: %v", err)
	}
}

func TestRunDeadEndEvidencePriority(t *testing.T) {
	t.Parallel()
	stderr, err := os.CreateTemp(t.TempDir(), "run-output")
	if err != nil {
		t.Fatal(err)
	}
	defer stderr.Close()
	failure := journal.Event{Kind: journal.KindFailure, Data: &journal.Failure{}, Time: time.Date(2026, 10, 2, 1, 14, 7, 0, time.UTC), Msg: "failure"}
	journalStream := journalStreamFor(t, stderr)
	renderer := journal.NewRenderer(stderr, func(k string) string {
		if k == "JOURNAL_STREAM" {
			return journalStream
		}
		return ""
	})
	stop := session.Stop{Reason: session.StopDeadEnd, DeadEnd: &journal.DeadEnd{Condition: journal.DeadEndSMU, Detail: "failed"}, Evidence: []journal.Event{failure}}
	if code := runResult(stop, nil, stderr, renderer); code != deadEndExit(stop.DeadEnd.Condition) {
		t.Fatalf("exit %d", code)
	}
	data, err := os.ReadFile(stderr.Name())
	if err != nil {
		t.Fatal(err)
	}
	if got := string(data); !strings.HasPrefix(got, "<3>\x1b[1;31mshycler: dead end smu: failed\x1b[0m\n") {
		t.Fatalf("summary not decorated as a red-bold priority-err line: %q", got)
	}
	if got := string(data); !strings.Contains(got, "\n<3>\x1b[31m  evidence: ") || !strings.Contains(got, "\x1b[0m\n") {
		t.Fatalf("evidence not decorated as a whole line: %q", got)
	}
}
