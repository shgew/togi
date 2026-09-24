package main

import (
	"bytes"
	"os"
	"path/filepath"
	"testing"
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
