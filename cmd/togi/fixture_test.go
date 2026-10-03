package main

import (
	"os"
	"path/filepath"
	"testing"
)

func installJournalFixture(t *testing.T, dir string) []byte {
	t.Helper()
	fixture, err := os.ReadFile("testdata/events.jsonl")
	if err != nil {
		t.Fatalf("read journal fixture: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), fixture, 0o644); err != nil {
		t.Fatalf("install journal fixture: %v", err)
	}
	return fixture
}
