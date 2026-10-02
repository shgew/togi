package main

import (
	"bytes"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/session"
)

func TestRunRefusesNewerEvidenceEpochBeforeConfig(t *testing.T) {
	dir := t.TempDir()
	build := session.Build()
	id := "20261001T000000Z"
	data := []byte(fmt.Sprintf(`{"seq":1,"kind":"session.start","session":%q,"schema":%d,"ruleset":%d,"evidence":%d,"version":"newer-build"}`+"\n", id, build.Schema, build.Ruleset, build.Epoch()+1))
	path := filepath.Join(dir, "events.jsonl")
	if err := os.WriteFile(path, data, 0o644); err != nil {
		t.Fatal(err)
	}
	config := filepath.Join(t.TempDir(), "invalid.toml")
	if err := os.WriteFile(config, []byte("removed_key = true\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	var stdout, stderr bytes.Buffer
	if code := cli([]string{"--state-dir", dir, "--config", config, "run"}, &stdout, &stderr); code != exitIncompatible || !strings.Contains(stderr.String(), "evidence epoch") || !strings.Contains(stderr.String(), "newer-build") || strings.Contains(stderr.String(), "removed_key") {
		t.Fatalf("newer epoch must refuse before config: exit %d, stderr %q", code, stderr.String())
	}
	after, err := os.ReadFile(path)
	if err != nil || !bytes.Equal(data, after) {
		t.Fatalf("refusal changed journal: %v", err)
	}
	if pending, err := journal.PendingCarry(dir); err != nil || pending != "" {
		t.Fatalf("refusal created pending carry %q: %v", pending, err)
	}
	if _, err := os.Stat(filepath.Join(dir, "archive", id+".jsonl")); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("newer epoch must never archive: %v", err)
	}
}
