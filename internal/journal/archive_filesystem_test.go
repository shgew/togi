package journal

import (
	"bytes"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"testing"
)

type resetArchiveFixture struct {
	dir, id  string
	j        *Journal
	faults   *faultJournalFilesystem
	original []byte
}

func resetArchiveSetup(t *testing.T) resetArchiveFixture {
	t.Helper()
	dir, j, faults, _, canonical := stateFailureFixture(t)
	if err := j.WriteState(canonical); err != nil {
		_ = j.Close()
		t.Fatal(err)
	}
	if _, err := j.Append(&CommandReset{Core: new(7)}); err != nil {
		_ = j.Close()
		t.Fatal(err)
	}
	data, err := os.ReadFile(filepath.Join(dir, eventsFile))
	if err != nil {
		_ = j.Close()
		t.Fatal(err)
	}
	if err := os.MkdirAll(filepath.Join(dir, trialsDir), 0o755); err != nil {
		_ = j.Close()
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, trialsDir, "artifact"), []byte("immutable trial log"), 0o644); err != nil {
		_ = j.Close()
		t.Fatal(err)
	}
	faults.calls = nil
	return resetArchiveFixture{dir: dir, id: j.Events()[0].Data.(*SessionStart).Session, j: j, faults: faults, original: data}
}

func TestResetArchiveFilesystemFailureReopenMatrix(t *testing.T) {
	reference := resetArchiveSetup(t)
	if _, err := reference.j.Archive(reference.id); err != nil {
		_ = reference.j.Close()
		t.Fatal(err)
	}
	steps := append([]journalFSCall(nil), reference.faults.calls...)
	if err := reference.j.Close(); err != nil {
		t.Fatal(err)
	}
	reference.resume(t)
	for i, step := range steps {
		for _, after := range []bool{false, true} {
			t.Run(fmt.Sprintf("%d %s after %v", i+1, step.op, after), func(t *testing.T) {
				fixture := resetArchiveSetup(t)
				fixture.faults.at, fixture.faults.after = i+1, after
				if _, err := fixture.j.Archive(fixture.id); !errors.Is(err, errJournalFilesystem) || !fixture.faults.fired {
					_ = fixture.j.Close()
					t.Fatalf("archive failure %v, fired %v", err, fixture.faults.fired)
				}
				if err := fixture.j.Close(); err != nil {
					t.Fatal(err)
				}
				fixture.resume(t)
			})
		}
	}
}

func (f resetArchiveFixture) resume(t *testing.T) {
	t.Helper()
	j, err := Open(f.dir, Options{Build: Build{Schema: Schema}})
	if err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(f.dir, archiveDir, f.id+".jsonl")
	if _, err := os.Stat(path); errors.Is(err, os.ErrNotExist) {
		if _, err := j.Archive(f.id); err != nil {
			_ = j.Close()
			t.Fatal(err)
		}
		if err := j.Close(); err != nil {
			t.Fatal(err)
		}
		j, err = Open(f.dir, Options{Build: Build{Schema: Schema}})
		if err != nil {
			t.Fatal(err)
		}
	} else if err != nil {
		_ = j.Close()
		t.Fatal(err)
	}
	data, err := os.ReadFile(path)
	if err != nil || !bytes.HasPrefix(data, f.original) {
		_ = j.Close()
		t.Fatalf("accepted source prefix changed: %v", err)
	}
	events, end, err := parse(data, Build{Schema: Schema})
	if err != nil || end != len(data) || countJournalKind(events, KindSessionArchived) != 1 || countJournalKind(events, KindCommandReset) != 1 {
		_ = j.Close()
		t.Fatalf("archive replay has duplicate or lost effects: %+v, %v", events, err)
	}
	artifact, err := os.ReadFile(filepath.Join(f.dir, archiveDir, f.id+trialsSuffix, "artifact"))
	if err != nil || string(artifact) != "immutable trial log" {
		_ = j.Close()
		t.Fatalf("trial log changed: %q, %v", artifact, err)
	}
	if len(j.Events()) != 0 {
		_ = j.Close()
		t.Fatal("reset retained the previous session")
	}
	start := sessionStart()
	start.Session = "next"
	appendAll(t, j, []Payload{start, &CommandReset{Core: new(7)}})
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	j, err = Open(f.dir, Options{Build: Build{Schema: Schema}})
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	if len(j.Events()) != 2 || countJournalKind(j.Events(), KindCommandReset) != 1 {
		t.Fatal("next-session reset duplicated across reopen")
	}
	if now, err := os.ReadFile(path); err != nil || !bytes.Equal(now, data) {
		t.Fatalf("archive changed after next-session reset: %v", err)
	}
}
