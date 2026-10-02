package journal

import (
	"bytes"
	"errors"
	"fmt"
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

func TestRecoverPendingArchiveFilesystemFailures(t *testing.T) {
	for at := 1; at <= 4; at++ {
		t.Run(fmt.Sprint(at), func(t *testing.T) {
			dir := t.TempDir()
			archive := filepath.Join(dir, archiveDir)
			if err := os.Mkdir(archive, 0755); err != nil {
				t.Fatal(err)
			}
			id := "source"
			path := filepath.Join(archive, id+".jsonl")
			data := []byte(`{"seq":1,"kind":"session.start","session":"source","schema":1,"ruleset":1}` + "\n")
			if err := os.WriteFile(path, data, 0600); err != nil {
				t.Fatal(err)
			}
			marker := filepath.Join(archive, id+"-compat-pending")
			if err := os.WriteFile(marker, nil, 0600); err != nil {
				t.Fatal(err)
			}
			j, err := Lock(dir, Options{Sync: true})
			if err != nil {
				t.Fatal(err)
			}
			defer j.Close()
			faults := &faultJournalFilesystem{journalFilesystem: j.fs, at: at}
			j.fs = faults
			if _, err := j.RecoverPendingArchive(); !errors.Is(err, errJournalFilesystem) {
				t.Fatalf("recovery failure: %v", err)
			}
			if at <= 3 {
				if _, err := os.Stat(marker); err != nil {
					t.Fatalf("pending marker lost before completion: %v", err)
				}
			}
			faults.at = 0
			got, err := j.RecoverPendingArchive()
			want := id
			if at == 4 {
				want = ""
			}
			if err != nil || got != want {
				t.Fatalf("recovery retry: %q, %v", got, err)
			}
			if _, err := os.Stat(marker); !errors.Is(err, os.ErrNotExist) {
				t.Fatalf("completed marker remains: %v", err)
			}
			preserved, err := os.ReadFile(path)
			if err != nil || !bytes.Equal(data, preserved) {
				t.Fatalf("archive changed: %q, %v", preserved, err)
			}
		})
	}
}

func TestPendingArchiveRejectsMissingSource(t *testing.T) {
	dir := t.TempDir()
	archive := filepath.Join(dir, archiveDir)
	if err := os.Mkdir(archive, 0755); err != nil {
		t.Fatal(err)
	}
	marker := filepath.Join(archive, "source-compat-pending")
	if err := os.WriteFile(marker, nil, 0600); err != nil {
		t.Fatal(err)
	}
	j, err := Open(dir, Options{Now: fixedClock()})
	if j != nil {
		j.Close()
	}
	if !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("missing archive accepted: %v", err)
	}
	if _, err := os.Stat(marker); err != nil {
		t.Fatalf("recovery evidence removed: %v", err)
	}
	if _, err := os.Stat(filepath.Join(dir, eventsFile)); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("new journal created over missing archive: %v", err)
	}
}

func TestRecordedIncompatibleArchiveCompletesWithoutCarry(t *testing.T) {
	for _, fail := range []bool{false, true} {
		for _, carry := range []bool{false, true} {
			t.Run(fmt.Sprintf("failure-%v-carry-%v", fail, carry), func(t *testing.T) {
				dir := t.TempDir()
				data := []byte(`{"seq":1,"kind":"session.start","session":"source","schema":99}` + "\n" +
					`{"seq":2,"kind":"session.archived","session":"source","path":"archive/source.jsonl"}` + "\n")
				path := filepath.Join(dir, eventsFile)
				if err := os.WriteFile(path, data, 0600); err != nil {
					t.Fatal(err)
				}
				j, err := Lock(dir, Options{Sync: true})
				if err != nil {
					t.Fatal(err)
				}
				defer j.Close()
				faults := &faultJournalFilesystem{journalFilesystem: j.fs}
				if fail {
					faults.at = 1
				}
				j.fs = faults
				archive := func() (string, error) {
					if carry {
						return j.ArchiveForCarry("source")
					}
					return j.ArchiveUnreadable("source")
				}
				if fail {
					if _, err := archive(); !errors.Is(err, errJournalFilesystem) {
						t.Fatalf("recorded archive failure: %v", err)
					}
					if preserved, err := os.ReadFile(path); err != nil || !bytes.Equal(preserved, data) {
						t.Fatalf("source lost before archive completion: %v", err)
					}
					faults.at = 0
				}
				rel, err := archive()
				want := "archive/source.jsonl"
				if carry {
					want = ""
				}
				if err != nil || rel != want {
					t.Fatalf("recorded archive completion: %q, %v", rel, err)
				}
				preserved, err := os.ReadFile(filepath.Join(dir, "archive", "source.jsonl"))
				if err != nil || !bytes.Equal(preserved, data) {
					t.Fatalf("incompatible source changed: %v", err)
				}
				if pending, err := PendingCarry(dir); err != nil || pending != "" {
					t.Fatalf("reset archive created carry: %q, %v", pending, err)
				}
				if _, err := os.Stat(filepath.Join(dir, "archive", "source-compat-pending")); !errors.Is(err, os.ErrNotExist) {
					t.Fatalf("already-recorded archive created compatibility marker: %v", err)
				}
			})
		}
	}
}

func TestArchiveForCarryKeepsPendingSourceAfterTornEstablishment(t *testing.T) {
	for _, kind := range []Kind{KindSessionContext, KindSessionCarried} {
		t.Run(string(kind), func(t *testing.T) {
			dir := t.TempDir()
			archive := filepath.Join(dir, archiveDir)
			if err := os.Mkdir(archive, 0755); err != nil {
				t.Fatal(err)
			}
			source := []byte(`{"seq":1,"kind":"session.start","session":"earlier","schema":2}` + "\n")
			if err := os.WriteFile(filepath.Join(archive, "earlier.jsonl"), source, 0600); err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(filepath.Join(archive, "earlier"+carrySuffix), nil, 0600); err != nil {
				t.Fatal(err)
			}
			live := []byte(`{"seq":1,"kind":"session.start","session":"interrupted","schema":2}` + "\n" + fmt.Sprintf(`{"seq":2,"kind":%q`, kind))
			if err := os.WriteFile(filepath.Join(dir, eventsFile), live, 0600); err != nil {
				t.Fatal(err)
			}
			j, err := Lock(dir, Options{})
			if err != nil {
				t.Fatal(err)
			}
			defer j.Close()
			if _, err := j.ArchiveForCarry("interrupted"); err != nil {
				t.Fatal(err)
			}
			pending, err := PendingCarry(dir)
			if err != nil || pending != "earlier" {
				t.Fatalf("interrupted establishment replaced pending source: %q, %v", pending, err)
			}
			preserved, err := os.ReadFile(filepath.Join(archive, "earlier.jsonl"))
			if err != nil || !bytes.Equal(source, preserved) {
				t.Fatalf("pending evidence changed: %q, %v", preserved, err)
			}
		})
	}
}
