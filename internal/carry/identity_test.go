package carry

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestArchiveSuffixesSortNumerically(t *testing.T) {
	dir := t.TempDir()
	archive := filepath.Join(dir, "archive")
	if err := os.MkdirAll(archive, 0o755); err != nil {
		t.Fatal(err)
	}
	for _, id := range []string{"20261001T235959Z", "20261002T000000Z", "20261002T000000Z-2", "20261002T000000Z-10", "20261002T000001Z"} {
		if err := os.WriteFile(filepath.Join(archive, id+".jsonl"), nil, 0o644); err != nil {
			t.Fatal(err)
		}
	}
	got, err := olderArchives(dir, "20261002T000001Z")
	if err != nil {
		t.Fatal(err)
	}
	want := []string{"20261002T000000Z-10", "20261002T000000Z-2", "20261002T000000Z", "20261001T235959Z"}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("archives newest first (-want +got):\n%s", diff)
	}
}
