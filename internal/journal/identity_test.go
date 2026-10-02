package journal

import (
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
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

func TestSessionIDsOrderNumericSuffixes(t *testing.T) {
	for _, tc := range []struct {
		a, b string
		want int
	}{
		{"20261002T000000Z-10", "20261002T000000Z-2", 1},
		{"20261002T000000Z-2", "20261002T000000Z-10", -1},
		{"20261002T000000Z", "20261002T000000Z-2", -1},
		{"20261002T000000Z-10", "20261002T000000Z-10", 0},
		{"20261001T000000Z-99", "20261002T000000Z", -1},
	} {
		if diff := cmp.Diff(tc.want, CompareSessionIDs(tc.a, tc.b)); diff != "" {
			t.Fatalf("%s versus %s: %s", tc.a, tc.b, diff)
		}
	}
}
