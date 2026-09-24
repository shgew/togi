package journal

import (
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestOpenFinishesArchive(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	j := openTest(t, dir)
	rel := filepath.Join("archive", "20261002T011407Z.jsonl")
	appendAll(t, j, []Payload{sessionStart(), &CommandReset{All: true}, &SessionArchived{Session: "20261002T011407Z", Path: rel}})
	if err := j.WriteState(State{}); err != nil {
		t.Fatal(err)
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(filepath.Join(dir, "trials", "0001"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "trials", "0001", "x"), []byte("x"), 0o644); err != nil {
		t.Fatal(err)
	}
	recorded, err := os.ReadFile(filepath.Join(dir, eventsFile))
	if err != nil {
		t.Fatal(err)
	}

	j = openTest(t, dir)
	defer j.Close()
	if n := len(j.Events()); n != 0 {
		t.Fatalf("reopened journal has %d events, want a fresh one", n)
	}
	if archived, err := os.ReadFile(filepath.Join(dir, rel)); err != nil || string(archived) != string(recorded) {
		t.Fatalf("archive: %v, %d bytes, want the %d recorded bytes", err, len(archived), len(recorded))
	}
	if _, err := os.Stat(filepath.Join(dir, stateFile)); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("state file after the archive: %v", err)
	}
	if x, err := os.ReadFile(filepath.Join(dir, "archive", "20261002T011407Z-trials", "0001", "x")); err != nil || string(x) != "x" {
		t.Fatalf("archived trial file: %v, %q", err, x)
	}
	if _, err := os.Stat(filepath.Join(dir, "trials")); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("trials directory after the archive: %v", err)
	}

	appendAll(t, j, []Payload{sessionStart()})
	if _, err := j.Archive("20261002T011407Z"); err == nil || !strings.Contains(err.Error(), "already exists") {
		t.Fatalf("archiving over an existing archive: %v", err)
	}
	if last := j.Events()[len(j.Events())-1]; last.Kind != KindSessionStart {
		t.Fatalf("refused archive recorded %s", last.Kind)
	}
}
