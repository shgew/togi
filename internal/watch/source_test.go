package watch

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

func watchSessionLine(t *testing.T, msg string) []byte {
	t.Helper()
	event := struct {
		Seq  int          `json:"seq"`
		Time time.Time    `json:"time"`
		Kind journal.Kind `json:"kind"`
		Msg  string       `json:"msg"`
		journal.SessionStart
	}{
		Seq: 1, Time: time.Unix(1000, 0).UTC(), Kind: journal.KindSessionStart, Msg: msg,
		Schema: journal.Schema, Ruleset: tuner.Ruleset, Session: "reload", Cores: []machine.CoreInfo{{Core: 0, CPUs: []int{0, 1}}},
	}
	data, err := json.Marshal(event)
	if err != nil {
		t.Fatal(err)
	}
	return append(data, '\n')
}

func watchWarningLine(t *testing.T, msg string) []byte {
	t.Helper()
	event := struct {
		Seq  int          `json:"seq"`
		Time time.Time    `json:"time"`
		Kind journal.Kind `json:"kind"`
		Msg  string       `json:"msg"`
		journal.SessionWarning
	}{Seq: 2, Time: time.Unix(1001, 0).UTC(), Kind: journal.KindSessionWarning, Msg: msg, Operation: "retain", Error: msg}
	data, err := json.Marshal(event)
	if err != nil {
		t.Fatal(err)
	}
	return append(data, '\n')
}

func watchFileInfo(t *testing.T, path string) os.FileInfo {
	t.Helper()
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	return info
}

func watchSourceFrame(t *testing.T, src *source, present, absent string) {
	t.Helper()
	snapshot := src.snapshot()
	if snapshot.Err() != nil {
		t.Fatal(snapshot.Err())
	}
	frame := Render(snapshot, 160, 40, time.Unix(1100, 0).UTC())
	if !strings.Contains(frame, present) || absent != "" && strings.Contains(frame, absent) {
		t.Fatalf("reload did not project current journal: want %q without %q:\n%s", present, absent, frame)
	}
}

func TestSourceReloadTransitions(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	path := filepath.Join(dir, "events.jsonl")
	stamp := time.Unix(2000, 0)
	write := func(path string, data []byte, mtime time.Time) {
		t.Helper()
		if err := os.WriteFile(path, data, 0644); err != nil {
			t.Fatal(err)
		}
		if err := os.Chtimes(path, mtime, mtime); err != nil {
			t.Fatal(err)
		}
	}
	write(path, watchSessionLine(t, "original"), stamp)
	src := source{dir: dir}
	watchSourceFrame(t, &src, "original", "no session yet")
	initial := watchFileInfo(t, path)

	replacement := filepath.Join(dir, "replacement")
	write(replacement, watchSessionLine(t, "replaced"), stamp)
	if err := os.Rename(replacement, path); err != nil {
		t.Fatal(err)
	}
	replaced := watchFileInfo(t, path)
	if os.SameFile(initial, replaced) || initial.Size() != replaced.Size() || !initial.ModTime().Equal(replaced.ModTime()) {
		t.Fatal("replacement did not isolate the changed source inode")
	}
	watchSourceFrame(t, &src, "replaced", "original")

	grown := append(watchSessionLine(t, "replaced"), watchWarningLine(t, "appended")...)
	write(path, grown, stamp)
	growth := watchFileInfo(t, path)
	if !os.SameFile(replaced, growth) || growth.Size() <= replaced.Size() || !growth.ModTime().Equal(replaced.ModTime()) {
		t.Fatal("append did not isolate the changed source size")
	}
	watchSourceFrame(t, &src, "appended", "original")

	modified := bytes.ReplaceAll(grown, []byte("appended"), []byte("modified"))
	write(path, modified, stamp.Add(time.Second))
	mtime := watchFileInfo(t, path)
	if !os.SameFile(growth, mtime) || growth.Size() != mtime.Size() || growth.ModTime().Equal(mtime.ModTime()) {
		t.Fatal("rewrite did not isolate the changed source modification time")
	}
	watchSourceFrame(t, &src, "modified", "appended")

	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}
	watchSourceFrame(t, &src, "no session yet", "modified")
	write(path, watchSessionLine(t, "restored"), stamp)
	watchSourceFrame(t, &src, "restored", "no session yet")
}

func TestLoadIncompatibleJournal(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	data := bytes.ReplaceAll(watchSessionLine(t, "newer schema"), []byte(fmt.Sprintf(`"schema":%d`, journal.Schema)), []byte(fmt.Sprintf(`"schema":%d`, journal.Schema+1)))
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), data, 0644); err != nil {
		t.Fatal(err)
	}
	s := Load(dir)
	var incompatible *journal.IncompatibleError
	if !errors.As(s.Err(), &incompatible) || s.session {
		t.Fatalf("incompatible journal presented as a session: %+v", s)
	}
}
