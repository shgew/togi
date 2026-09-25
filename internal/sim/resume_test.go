package sim

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestResumeAfterIncompatibleArchive(t *testing.T) {
	dir := t.TempDir()
	archive := filepath.Join(dir, "archive")
	if err := os.Mkdir(archive, 0o755); err != nil {
		t.Fatal(err)
	}
	data := []byte(`{"seq":1,"time":"2026-10-02T01:14:07Z","boot":"old","kind":"session.start","session":"old","schema":99}` + "\n" +
		`{"seq":2,"time":"2026-10-02T01:14:08Z","boot":"old","kind":"later.unknown"}` + "\n")
	if err := os.WriteFile(filepath.Join(archive, "old.jsonl"), data, 0o644); err != nil {
		t.Fatal(err)
	}
	got, err := Resume(dir, Config{Seed: 1})
	want := time.Date(2026, 10, 2, 1, 14, 8, 0, time.UTC).Add(RebootTime)
	if err != nil || got.Boots != 1 || !got.Start.Equal(want) {
		t.Fatalf("resume archived schema: %+v, %v; want one boot and start %s", got, err, want)
	}
}
