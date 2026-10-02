package journal

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestSessionIdentityReservesArchivedTrialDirectories(t *testing.T) {
	dir := t.TempDir()
	archive := filepath.Join(dir, archiveDir)
	for _, id := range []string{"20261002T000000Z", "20261002T000000Z-3"} {
		if err := os.MkdirAll(filepath.Join(archive, id+trialsSuffix), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.WriteFile(filepath.Join(archive, "20261002T000000Z-2.jsonl"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
	j, err := Lock(dir, Options{})
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	id, err := j.SessionID(time.Date(2026, 10, 2, 0, 0, 0, 0, time.UTC))
	if err != nil || id != "20261002T000000Z-4" {
		t.Fatalf("session identity %q, %v; want -4", id, err)
	}
	if _, err := j.ArchivePath(id); err != nil {
		t.Fatalf("allocated identity cannot archive: %v", err)
	}
}
