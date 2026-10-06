package journal

import (
	"errors"
	"io"
	"io/fs"
	"path/filepath"
	"testing"
)

var errJournalFilesystem = errors.New("injected journal filesystem failure")

type journalFSCall struct {
	op, path, target string
}

type faultJournalFilesystem struct {
	journalFilesystem
	calls        []journalFSCall
	at           int
	failOp       string
	after, fired bool
	shortPath    string
	shortFired   bool
}

func (f *faultJournalFilesystem) operation(call journalFSCall, effect func() error) error {
	f.calls = append(f.calls, call)
	fail := !f.fired && (f.at > 0 && len(f.calls) == f.at || f.failOp != "" && call.op == f.failOp)
	if fail && !f.after {
		f.fired = true
		return errJournalFilesystem
	}
	err := effect()
	if fail {
		f.fired = true
		return errJournalFilesystem
	}
	return err
}

func (f *faultJournalFilesystem) OpenFile(path string, flags int, mode fs.FileMode) (journalFile, error) {
	var file journalFile
	err := f.operation(journalFSCall{op: "open", path: path}, func() error {
		var err error
		file, err = f.journalFilesystem.OpenFile(path, flags, mode)
		return err
	})
	if err != nil {
		if file != nil {
			_ = file.Close()
		}
		return nil, err
	}
	return faultJournalFile{journalFile: file, fs: f, path: path}, nil
}

func (f *faultJournalFilesystem) Rename(from, to string) error {
	return f.operation(journalFSCall{op: "rename", path: from, target: to}, func() error { return f.journalFilesystem.Rename(from, to) })
}

func (f *faultJournalFilesystem) Remove(path string) error {
	return f.operation(journalFSCall{op: "remove", path: path}, func() error { return f.journalFilesystem.Remove(path) })
}

func (f *faultJournalFilesystem) SyncDir(path string) error {
	return f.operation(journalFSCall{op: "directory sync", path: path}, func() error { return nil })
}

type faultJournalFile struct {
	journalFile
	fs   *faultJournalFilesystem
	path string
}

func (f faultJournalFile) Write(data []byte) (int, error) {
	n := 0
	err := f.fs.operation(journalFSCall{op: "write", path: f.path}, func() error {
		if !f.fs.shortFired && filepath.Base(f.path) == f.fs.shortPath {
			f.fs.shortFired = true
			var err error
			n, err = f.journalFile.Write(data[:len(data)/2])
			return err
		}
		var err error
		n, err = f.journalFile.Write(data)
		return err
	})
	return n, err
}

func (f faultJournalFile) Sync() error {
	return f.fs.operation(journalFSCall{op: "file sync", path: f.path}, func() error { return nil })
}

func openFaultJournal(t *testing.T, dir string) (*Journal, *faultJournalFilesystem) {
	t.Helper()
	j, err := Lock(dir, Options{Boot: "persistence", Now: fixedClock(), Build: Build{Schema: Schema}, Sync: true})
	if err != nil {
		t.Fatal(err)
	}
	faults := &faultJournalFilesystem{journalFilesystem: j.fs}
	j.fs = faults
	if err := j.Open(); err != nil {
		_ = j.Close()
		t.Fatal(err)
	}
	faults.calls = nil
	return j, faults
}

func TestFilesystemShortWritesNeverCommit(t *testing.T) {
	t.Run("append", func(t *testing.T) {
		dir := t.TempDir()
		j, faults := openFaultJournal(t, dir)
		appendAll(t, j, []Payload{sessionStart()})
		faults.shortPath = eventsFile
		if _, err := j.Append(&CommandReset{Core: new(7)}); !errors.Is(err, io.ErrShortWrite) {
			t.Fatalf("partial event committed: %v", err)
		}
		if _, err := j.Append(&Shutdown{Reason: ShutdownCommand}); !errors.Is(err, io.ErrShortWrite) {
			t.Fatalf("writer continued after uncertain append: %v", err)
		}
		if err := j.Close(); err != nil {
			t.Fatal(err)
		}
		j, err := Open(dir, Options{Build: Build{Schema: Schema}, Sync: true})
		if err != nil {
			t.Fatal(err)
		}
		defer j.Close()
		for _, event := range j.Events() {
			if event.Kind == KindCommandReset {
				t.Fatal("partial reset replayed")
			}
		}
		if _, err := j.Append(&CommandReset{Core: new(7)}); err != nil {
			t.Fatal(err)
		}
	})
	t.Run("state", func(t *testing.T) {
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
		faults.shortPath = stateTmpFile
		if err := j.WriteState(canonical); !errors.Is(err, io.ErrShortWrite) {
			t.Fatalf("partial state committed: %v", err)
		}
		stored, err := j.ReadState()
		if err != nil || stored.LastSeq != previous.LastSeq || stored.Session.Baseline[0] != -5 {
			t.Fatalf("previous complete state replaced: %+v, %v", stored, err)
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
		if err != nil || stored.LastSeq != canonical.LastSeq || stored.Session.Baseline[0] != -7 {
			t.Fatalf("torn temp became authoritative: %+v, %v", stored, err)
		}
	})
}

func TestAppendSyncFailureRequiresReopen(t *testing.T) {
	dir := t.TempDir()
	j, faults := openFaultJournal(t, dir)
	appendAll(t, j, []Payload{sessionStart()})
	faults.at = len(faults.calls) + 2
	if _, err := j.Append(&CommandReset{Core: new(7)}); !errors.Is(err, errJournalFilesystem) {
		t.Fatalf("append fsync failure: %v", err)
	}
	if len(j.Events()) != 1 {
		t.Fatal("uncertain append admitted to memory")
	}
	calls := len(faults.calls)
	if _, err := j.Append(&Shutdown{Reason: ShutdownCommand}); !errors.Is(err, errJournalFilesystem) {
		t.Fatalf("writer continued with stale sequence after failed fsync: %v", err)
	}
	if len(faults.calls) != calls {
		t.Fatal("poisoned writer issued more filesystem operations")
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	j, err := Open(dir, Options{Build: Build{Schema: Schema}, Sync: true})
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	if len(j.Events()) != 2 || j.Events()[1].Kind != KindCommandReset {
		t.Fatalf("complete line surviving failed fsync not replayed: %+v", j.Events())
	}
	event, err := j.Append(&Shutdown{Reason: ShutdownCommand}, 2)
	if err != nil || event.Seq != 3 {
		t.Fatalf("sequence after reopen: #%d, %v", event.Seq, err)
	}
}

type failingJournalFile struct {
	journalFile
	closeErr error
}

func (f failingJournalFile) Close() error {
	err := f.journalFile.Close()
	return errors.Join(err, f.closeErr)
}

type failingJournalFilesystem struct {
	journalFilesystem
	closeErr error
}

func (f failingJournalFilesystem) OpenFile(path string, flags int, mode fs.FileMode) (journalFile, error) {
	file, err := f.journalFilesystem.OpenFile(path, flags, mode)
	if err != nil {
		return nil, err
	}
	return failingJournalFile{journalFile: file, closeErr: f.closeErr}, nil
}
