package journal

import (
	"bytes"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"
)

func TestAppendFilesystemFailureReopenMatrix(t *testing.T) {
	for at := 1; at <= 2; at++ {
		for _, after := range []bool{false, true} {
			t.Run(fmt.Sprintf("operation %d after %v", at, after), func(t *testing.T) {
				dir := t.TempDir()
				j, faults := openFaultJournal(t, dir)
				appendAll(t, j, []Payload{sessionStart()})
				prefix, err := os.ReadFile(filepath.Join(dir, eventsFile))
				if err != nil {
					t.Fatal(err)
				}
				faults.calls, faults.at, faults.after = nil, at, after
				if _, err := j.Append(&CommandReset{Core: new(7)}); !errors.Is(err, errJournalFilesystem) || !faults.fired {
					t.Fatalf("append failure %v, fired %v", err, faults.fired)
				}
				if _, err := j.Append(&Shutdown{Reason: ShutdownCommand}); !errors.Is(err, errJournalFilesystem) {
					t.Fatalf("uncertain writer continued: %v", err)
				}
				if err := j.Close(); err != nil {
					t.Fatal(err)
				}
				j, err = Open(dir, Options{Build: Build{Schema: Schema}, Sync: true})
				if err != nil {
					t.Fatal(err)
				}
				defer j.Close()
				survived := at != 1 || after
				resets := countJournalKind(j.Events(), KindCommandReset)
				if resets != map[bool]int{false: 0, true: 1}[survived] {
					t.Fatalf("reset survival after uncertain append: %d", resets)
				}
				if !survived {
					if _, err := j.Append(&CommandReset{Core: new(7)}); err != nil {
						t.Fatal(err)
					}
				}
				if countJournalKind(j.Events(), KindCommandReset) != 1 {
					t.Fatal("reset lost or applied twice")
				}
				if _, err := j.Append(&Shutdown{Reason: ShutdownCommand}, len(j.Events())); err != nil {
					t.Fatal(err)
				}
				data, err := os.ReadFile(filepath.Join(dir, eventsFile))
				if err != nil || !bytes.HasPrefix(data, prefix) {
					t.Fatalf("accepted prefix changed: %v", err)
				}
				for i, event := range j.Events() {
					if event.Seq != i+1 {
						t.Fatalf("sequence #%d at index %d", event.Seq, i)
					}
				}
			})
		}
	}
}

func countJournalKind(events []Event, kind Kind) int {
	n := 0
	for _, event := range events {
		if event.Kind == kind {
			n++
		}
	}
	return n
}

func stateFailureFixture(t *testing.T) (string, *Journal, *faultJournalFilesystem, State, State) {
	t.Helper()
	dir := t.TempDir()
	j, faults := openFaultJournal(t, dir)
	appendAll(t, j, []Payload{sessionStart(), &SessionBaseline{Offsets: []int{-5}}})
	var previous State
	Replay(j.Events(), &previous)
	if err := j.WriteState(previous); err != nil {
		t.Fatal(err)
	}
	appendAll(t, j, []Payload{&SessionBaseline{Offsets: []int{-7}}})
	var canonical State
	Replay(j.Events(), &canonical)
	faults.calls = nil
	return dir, j, faults, previous, canonical
}

func TestStateFilesystemFailureReopenMatrix(t *testing.T) {
	_, reference, probe, _, canonical := stateFailureFixture(t)
	if err := reference.WriteState(canonical); err != nil {
		t.Fatal(err)
	}
	steps := append([]journalFSCall(nil), probe.calls...)
	if err := reference.Close(); err != nil {
		t.Fatal(err)
	}
	for i, step := range steps {
		for _, after := range []bool{false, true} {
			t.Run(fmt.Sprintf("%d %s after %v", i+1, step.op, after), func(t *testing.T) {
				testStateFailureReopen(t, i+1, after)
			})
		}
	}
}

func testStateFailureReopen(t *testing.T, at int, after bool) {
	t.Helper()
	dir, j, faults, previous, canonical := stateFailureFixture(t)
	faults.at, faults.after = at, after
	if err := j.WriteState(canonical); !errors.Is(err, errJournalFilesystem) || !faults.fired {
		t.Fatalf("projection failure %v, fired %v", err, faults.fired)
	}
	stored, err := j.ReadState()
	if err != nil {
		t.Fatalf("projection became torn: %v", err)
	}
	if diffOld, diffNew := cmp.Diff(previous, stored, cmpopts.IgnoreUnexported(State{})), cmp.Diff(canonical, stored, cmpopts.IgnoreUnexported(State{})); diffOld != "" && diffNew != "" {
		t.Fatalf("neither complete projection survived:\n%s\n%s", diffOld, diffNew)
	}
	if _, err := j.Append(&SessionWarning{Operation: "write state projection", Error: errJournalFilesystem.Error()}, canonical.LastSeq); err != nil {
		t.Fatalf("projection fault poisoned authoritative journal: %v", err)
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	j, err = Open(dir, Options{Build: Build{Schema: Schema}, Sync: true})
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	var rebuilt State
	Replay(j.Events(), &rebuilt)
	if err := j.WriteState(rebuilt); err != nil {
		t.Fatal(err)
	}
	stored, err = j.ReadState()
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(rebuilt, stored, cmpopts.IgnoreUnexported(State{})); diff != "" {
		t.Fatalf("replay did not replace stale state (-canonical +stored):\n%s", diff)
	}
	if stored.Session.Baseline[0] != -7 {
		t.Fatal("torn temp overrode canonical baseline")
	}
}
