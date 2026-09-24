package main

import (
	"bytes"
	"os"
	"path/filepath"
	"testing"

	"code.marleb.org/shgew/shycler/internal/journal"
)

func TestCommandsRefuseWhileLocked(t *testing.T) {
	fixture, err := os.ReadFile("testdata/events.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), fixture, 0o644); err != nil {
		t.Fatal(err)
	}
	j, err := journal.Open(dir, journal.Options{Boot: "run"})
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	for _, args := range [][]string{{"regain"}, {"reset", "--core", "3"}, {"reset", "--all"}} {
		var stdout, stderr bytes.Buffer
		if code := cli(append([]string{"--state-dir", dir}, args...), &stdout, &stderr); code != exitLocked {
			t.Errorf("%v: exit %d, want %d; stderr %q", args, code, exitLocked, stderr.String())
		}
	}
	for _, args := range [][]string{{"reset"}, {"reset", "--core", "3", "--all"}} {
		var stdout, stderr bytes.Buffer
		if code := cli(append([]string{"--state-dir", dir}, args...), &stdout, &stderr); code != exitUsage {
			t.Errorf("%v: exit %d, want %d", args, code, exitUsage)
		}
	}
	after, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
	if err != nil || !bytes.Equal(after, fixture) {
		t.Fatalf("journal changed while locked: %v", err)
	}
}
