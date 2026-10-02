package carry

import (
	"bytes"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"

	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
)

func TestCompetingTransitionCannotClearCarry(t *testing.T) {
	dir := t.TempDir()
	old := newJournal(t, dir, "A", 3, &context)
	old.fail(0, -20, "isolated", journal.Attributed)
	old.close()
	first := prepare(t, dir, []defect.Entry{})
	winner := newJournal(t, dir, "B", 4, &context)
	winner.add(&journal.SessionCarried{Sources: first.Sources, Marks: true, Carried: first.Cores})
	done := make(chan error, 1)
	go func() {
		_, err := prepareWithContext(dir, []defect.Entry{}, nil)
		done <- err
	}()
	if err := <-done; !errors.Is(err, journal.ErrLocked) {
		t.Fatalf("competing transition: %v, want journal.ErrLocked", err)
	}
	if _, err := os.Stat(filepath.Join(dir, "archive", "A-carry-pending")); err != nil {
		t.Fatalf("competing transition removed winner's carry marker: %v", err)
	}
}

func TestTransitionLockPrecedesPreparation(t *testing.T) {
	dir := t.TempDir()
	old := newJournal(t, dir, "A", 3, &context)
	old.fail(0, -20, "isolated", journal.Attributed)
	old.close()
	path := filepath.Join(dir, "events.jsonl")
	before, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	locked, release, done := make(chan struct{}), make(chan struct{}), make(chan error, 1)
	go func() {
		j, err := journal.Lock(dir, opts())
		if err != nil {
			done <- err
			close(locked)
			return
		}
		defer j.Close()
		close(locked)
		<-release
		_, err = Prepare(j, binary, []defect.Entry{}, nil)
		done <- err
	}()
	<-locked
	_, losingErr := prepareWithContext(dir, []defect.Entry{}, nil)
	after, readErr := os.ReadFile(path)
	close(release)
	winnerErr := <-done
	if !errors.Is(losingErr, journal.ErrLocked) {
		t.Fatalf("losing transition: %v", losingErr)
	}
	if readErr != nil || !bytes.Equal(before, after) {
		t.Fatalf("losing transition changed original journal: %v", readErr)
	}
	if winnerErr != nil {
		t.Fatal(winnerErr)
	}
	archived, err := os.ReadFile(filepath.Join(dir, "archive", "A.jsonl"))
	if err != nil || !bytes.Equal(before, archived) {
		t.Fatalf("winning transition changed archive: %v", err)
	}
	markers, err := filepath.Glob(filepath.Join(dir, "archive", "*-carry-pending"))
	if err != nil || len(markers) != 1 {
		t.Fatalf("carry markers: %v, %v", markers, err)
	}
}

func TestTransitionRefusesCorruptCurrentSchemaWithoutMutation(t *testing.T) {
	for _, corruption := range []string{"malformed complete line", "discontinuous sequence"} {
		t.Run(corruption, func(t *testing.T) {
			dir := t.TempDir()
			old := newJournal(t, dir, "A", 3, &context)
			old.fail(0, -20, "isolated", journal.Attributed)
			old.close()
			path := filepath.Join(dir, "events.jsonl")
			data, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			switch corruption {
			case "malformed complete line":
				data = append(data, "{\"seq\":6,\"kind\":\"trial.end\"\n"...)
			case "discontinuous sequence":
				data = bytes.Replace(data, []byte(`"seq":3`), []byte(`"seq":99`), 1)
			}
			if err := os.WriteFile(path, data, 0o644); err != nil {
				t.Fatal(err)
			}
			for _, file := range []string{"trials/0001/result", "state.json", "archive/previous.jsonl", "archive/previous-carry-pending"} {
				path := filepath.Join(dir, file)
				if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
					t.Fatal(err)
				}
				if err := os.WriteFile(path, []byte(file), 0o644); err != nil {
					t.Fatal(err)
				}
			}
			snapshot := func() map[string]string {
				t.Helper()
				files := map[string]string{}
				for _, root := range []string{"events.jsonl", "trials", "state.json", "archive"} {
					err := filepath.WalkDir(filepath.Join(dir, root), func(path string, entry fs.DirEntry, err error) error {
						if errors.Is(err, fs.ErrNotExist) {
							return nil
						}
						if err != nil {
							return err
						}
						rel, err := filepath.Rel(dir, path)
						if err != nil {
							return err
						}
						if entry.IsDir() {
							files[rel] = "<directory>"
							return nil
						}
						data, err := os.ReadFile(path)
						if err != nil {
							return err
						}
						files[rel] = string(data)
						return nil
					})
					if err != nil {
						t.Fatal(err)
					}
				}
				return files
			}
			before := snapshot()
			if carried, err := prepareWithContext(dir, []defect.Entry{}, nil); err == nil {
				t.Errorf("corrupt current-schema transition accepted: %+v", carried)
			}
			if diff := cmp.Diff(before, snapshot()); diff != "" {
				t.Errorf("corrupt transition mutated evidence (-before +after):\n%s", diff)
			}
		})
	}
}
