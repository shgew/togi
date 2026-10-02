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
				j, err = Open(dir, Options{Build: Build{Schema: Schema}})
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
	j, err = Open(dir, Options{Build: Build{Schema: Schema}})
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

func TestOpenRepairFilesystemFailures(t *testing.T) {
	for at := 1; at <= 5; at++ {
		for _, after := range []bool{false, true} {
			t.Run(fmt.Sprintf("operation-%d-after-%v", at, after), func(t *testing.T) {
				if at == 4 && !after {
					t.Skip("known pre-existing evidence-loss bug: tail is truncated before the journal.torn write; https://github.com/shgew/togi/issues/305")
				}
				dir := t.TempDir()
				prefix := []byte(`{"seq":1,"kind":"session.start","schema":2}` + "\n")
				tail := []byte(`{"seq":2,"kind":"shutdown"`)
				path := filepath.Join(dir, eventsFile)
				if err := os.WriteFile(path, append(bytes.Clone(prefix), tail...), 0600); err != nil {
					t.Fatal(err)
				}
				j, err := Lock(dir, Options{Now: fixedClock(), Sync: true})
				if err != nil {
					t.Fatal(err)
				}
				faults := &faultJournalFilesystem{journalFilesystem: j.fs, at: at, after: after}
				j.fs = faults
				if err := j.Open(); !errors.Is(err, errJournalFilesystem) || !faults.fired {
					j.Close()
					t.Fatalf("repair failure = %v, fired %v", err, faults.fired)
				}
				j.Close()
				j, err = Open(dir, Options{Now: fixedClock(), Sync: true})
				if err != nil {
					t.Fatal(err)
				}
				defer j.Close()
				data, err := os.ReadFile(path)
				if err != nil || !bytes.HasPrefix(data, prefix) {
					t.Fatalf("accepted prefix changed: %q, %v", data, err)
				}
				events, torn, err := Read(dir)
				if err != nil || len(torn) != 0 {
					t.Fatalf("repair left torn tail: %q, %v", torn, err)
				}
				if len(events) != 2 {
					t.Fatalf("discarded-byte evidence missing: got %d events, want session.start and journal.torn", len(events))
				}
				p, ok := events[1].Data.(*JournalTorn)
				if !ok || p.BytesHex != fmt.Sprintf("%x", tail) || p.Offset != int64(len(prefix)) {
					t.Fatalf("discarded-byte evidence: %+v", events[1])
				}
				e, err := j.Append(&Shutdown{Reason: ShutdownCommand})
				if err != nil || e.Seq != len(events)+1 {
					t.Fatalf("recovered sequence: %+v, %v", e, err)
				}
			})
		}
	}
}

func TestTornTailTruncateFailurePreservesEvidence(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, eventsFile)
	data := []byte(`{"seq":1,"kind":"session.start","schema":2}` + "\n" + `{"seq":2`)
	if err := os.WriteFile(path, data, 0600); err != nil {
		t.Fatal(err)
	}
	j, err := Lock(dir, Options{Now: fixedClock()})
	if err != nil {
		t.Fatal(err)
	}
	j.fs = failingJournalFilesystem{journalFilesystem: j.fs, truncateErr: errJournalFilesystem}
	if err := j.Open(); !errors.Is(err, errJournalFilesystem) {
		t.Fatalf("truncate failure: %v", err)
	}
	j.Close()
	after, err := os.ReadFile(path)
	if err != nil || !bytes.Equal(data, after) {
		t.Fatalf("failed truncate changed evidence: %q, %v", after, err)
	}
	j = openTest(t, dir)
	if countJournalKind(j.Events(), KindJournalTorn) != 1 {
		t.Fatal("retry did not record discarded evidence")
	}
}

func TestTornFirstLineSyncFailure(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, eventsFile)
	if err := os.WriteFile(path, []byte(`{"seq":1`), 0600); err != nil {
		t.Fatal(err)
	}
	j, err := Lock(dir, Options{Now: fixedClock(), Sync: true})
	if err != nil {
		t.Fatal(err)
	}
	j.fs = &faultJournalFilesystem{journalFilesystem: j.fs, at: 4}
	if err := j.Open(); !errors.Is(err, errJournalFilesystem) {
		t.Fatalf("empty repair sync failure: %v", err)
	}
	j.Close()
	j = openTest(t, dir)
	if len(j.Events()) != 0 {
		t.Fatal("torn first line manufactured an event")
	}
	events := appendAll(t, j, []Payload{sessionStart()})
	if events[0].Seq != 1 {
		t.Fatal("torn first line consumed sequence")
	}
}
