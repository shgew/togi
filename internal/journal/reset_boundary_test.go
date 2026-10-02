package journal

import (
	"errors"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestResetBoundaryPersistsBeforePendingCarryRemoval(t *testing.T) {
	dir := t.TempDir()
	j, faults := openFaultJournal(t, dir)
	defer j.Close()
	id := "20261002T000000Z"
	appendAll(t, j, []Payload{&SessionStart{Session: id, Schema: Schema}})
	archive := filepath.Join(dir, archiveDir)
	if err := os.MkdirAll(archive, 0o755); err != nil {
		t.Fatal(err)
	}
	pending := filepath.Join(archive, id+carrySuffix)
	if err := os.WriteFile(pending, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	faults.calls = nil
	// Fail syncing the boundary file, before removal of the pending source.
	faults.at = 2
	if _, err := j.DropPendingCarry(); !errors.Is(err, errJournalFilesystem) {
		t.Fatalf("expected reset boundary sync failure, got %v", err)
	}
	if _, err := os.Stat(pending); err != nil {
		t.Fatalf("pending evidence removed before durable reset boundary: %v", err)
	}
	faults.at = 0
	if got, err := j.DropPendingCarry(); err != nil || got != id {
		t.Fatalf("reset retry: source %q, error %v", got, err)
	}
	if got, err := ResetBoundary(dir); err != nil || got != id {
		t.Fatalf("durable reset boundary %q, error %v", got, err)
	}
	if _, err := os.Stat(pending); !os.IsNotExist(err) {
		t.Fatalf("pending evidence remained after durable reset: %v", err)
	}
}

func TestSessionIDAlwaysFollowsResetBoundary(t *testing.T) {
	for _, now := range []time.Time{
		time.Date(2026, 10, 2, 0, 0, 0, 0, time.UTC),
		time.Date(2026, 10, 1, 0, 0, 0, 0, time.UTC),
	} {
		t.Run(now.Format(time.RFC3339), func(t *testing.T) {
			dir := t.TempDir()
			j, err := Lock(dir, Options{Sync: true})
			if err != nil {
				t.Fatal(err)
			}
			defer j.Close()
			archive := filepath.Join(dir, archiveDir)
			if err := os.MkdirAll(archive, 0o755); err != nil {
				t.Fatal(err)
			}
			// Numeric suffix order matters and sources can have disappeared.
			for _, id := range []string{"20261002T000000Z-2", "20261002T000000Z-10"} {
				if err := os.WriteFile(filepath.Join(archive, id+carrySuffix), nil, 0o644); err != nil {
					t.Fatal(err)
				}
			}
			if err := j.MarkResetAll(); err != nil {
				t.Fatal(err)
			}
			if err := j.ClearPendingCarry(); err != nil {
				t.Fatal(err)
			}
			got, err := j.SessionID(now)
			if err != nil || got != "20261002T000000Z-11" {
				t.Fatalf("new session behind dropped pending source: id %q, error %v", got, err)
			}
		})
	}
}

func TestResetBoundaryRejectsSymlinks(t *testing.T) {
	dir := t.TempDir()
	j, err := Lock(dir, Options{Sync: true})
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	archive := filepath.Join(dir, archiveDir)
	if err := os.MkdirAll(archive, 0o755); err != nil {
		t.Fatal(err)
	}
	id := "20261002T000000Z"
	if err := os.WriteFile(filepath.Join(archive, id+carrySuffix), nil, 0o644); err != nil {
		t.Fatal(err)
	}
	target := filepath.Join(t.TempDir(), "target")
	if err := os.WriteFile(target, []byte("must not change"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(target, filepath.Join(archive, id+resetAllSuffix)); err != nil {
		t.Fatal(err)
	}
	if err := j.MarkResetAll(); err == nil {
		t.Fatal("reset followed a boundary symlink")
	}
	if _, err := ResetBoundary(dir); err == nil {
		t.Fatal("carry accepted a boundary symlink")
	}
	got, err := os.ReadFile(target)
	if err != nil || string(got) != "must not change" {
		t.Fatalf("reset changed symlink target: %q, error %v", got, err)
	}
}
