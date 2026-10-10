package watch

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/charmbracelet/colorprofile"
	"github.com/charmbracelet/x/ansi"
	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

// watchSessionLine is a session start followed by a warning carrying msg, which the dashboard shows verbatim.
func watchSessionLine(t *testing.T, msg string) []byte {
	t.Helper()
	event := struct {
		Seq  int          `json:"seq"`
		Time time.Time    `json:"time"`
		Kind journal.Kind `json:"kind"`
		Msg  string       `json:"msg"`
		journal.SessionStart
	}{
		Seq: 1, Time: time.Unix(1000, 0).UTC(), Kind: journal.KindSessionStart, Msg: "session reload started",
		Schema: journal.Schema, Ruleset: tuner.Ruleset, Session: "reload", Cores: []machine.CoreInfo{{Core: 0, CPUs: []int{0, 1}}},
	}
	data, err := json.Marshal(event)
	if err != nil {
		t.Fatal(err)
	}
	return append(append(data, '\n'), warningLine(t, 2, msg)...)
}

func watchWarningLine(t *testing.T, msg string) []byte {
	t.Helper()
	return warningLine(t, 3, msg)
}

func warningLine(t *testing.T, seq int, msg string) []byte {
	t.Helper()
	event := struct {
		Seq  int          `json:"seq"`
		Time time.Time    `json:"time"`
		Kind journal.Kind `json:"kind"`
		Msg  string       `json:"msg"`
		journal.SessionWarning
	}{Seq: seq, Time: time.Unix(1000+int64(seq), 0).UTC(), Kind: journal.KindSessionWarning, Msg: msg, Operation: "retain", Error: msg}
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

func TestJournalNotificationsReloadChanges(t *testing.T) {
	t.Parallel()
	if runtime.GOOS != "linux" {
		t.Skip("journal notifications need Linux")
	}
	for _, operation := range []string{"append", "replace", "delete", "create", "missing directory", "replaced directory", "traverse-only append", "traverse-only replace"} {
		t.Run(operation, func(t *testing.T) {
			root := t.TempDir()
			dir := filepath.Join(root, "state", "nested")
			path := filepath.Join(dir, "events.jsonl")
			if operation != "missing directory" {
				if err := os.MkdirAll(dir, 0755); err != nil {
					t.Fatal(err)
				}
			}
			if operation != "create" && operation != "missing directory" {
				if err := os.WriteFile(path, watchSessionLine(t, "original"), 0644); err != nil {
					t.Fatal(err)
				}
			}
			if strings.HasPrefix(operation, "traverse-only ") {
				// A root run leaves the state directory 0711; without read permission it cannot be watched.
				if err := os.Chmod(dir, 0311); err != nil {
					t.Fatal(err)
				}
				t.Cleanup(func() { _ = os.Chmod(dir, 0755) })
			}
			src := source{dir: dir}
			src.reload()
			changes, _, stop, err := watchJournal(context.Background(), dir, func() {})
			if err != nil {
				t.Fatal(err)
			}
			defer stop()
			switch strings.TrimPrefix(operation, "traverse-only ") {
			case "append":
				file, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND, 0)
				if err != nil {
					t.Fatal(err)
				}
				_, err = file.Write(watchWarningLine(t, "updated"))
				closeErr := file.Close()
				if err != nil || closeErr != nil {
					t.Fatalf("append journal: %v; close: %v", err, closeErr)
				}
			case "replace":
				replacement := filepath.Join(root, "replacement")
				if err := os.WriteFile(replacement, watchSessionLine(t, "updated"), 0644); err != nil {
					t.Fatal(err)
				}
				if err := os.Rename(replacement, path); err != nil {
					t.Fatal(err)
				}
			case "delete":
				if err := os.Remove(path); err != nil {
					t.Fatal(err)
				}
			case "replaced directory":
				if err := os.Rename(dir, dir+"-old"); err != nil {
					t.Fatal(err)
				}
				fallthrough
			case "create", "missing directory":
				if err := os.MkdirAll(dir, 0755); err != nil {
					t.Fatal(err)
				}
				if err := os.WriteFile(path, watchSessionLine(t, "updated"), 0644); err != nil {
					t.Fatal(err)
				}
			}
			if err, ok := <-changes; !ok || err != nil {
				t.Fatalf("journal notification: open=%t err=%v", ok, err)
			}
			if !src.reload() {
				t.Fatal("notification did not reload the changed journal")
			}
			want := "updated"
			if operation == "delete" {
				want = "no session yet"
			}
			text := Render(src.snap, 160, 40, time.Unix(1100, 0).UTC())
			if !strings.Contains(text, want) {
				t.Fatalf("%s notification did not show %q:\n%s", operation, want, text)
			}
		})
	}
}

func TestSourceReloadGuard(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	src := source{dir: dir}
	if !src.reload() || src.reload() {
		t.Fatal("unchanged missing journal was repeatedly reloaded")
	}
	path := filepath.Join(dir, "events.jsonl")
	if err := os.WriteFile(path, watchSessionLine(t, "original"), 0644); err != nil {
		t.Fatal(err)
	}
	if !src.reload() || src.reload() {
		t.Fatal("unchanged journal was repeatedly reloaded")
	}
}

func TestLiveJournalProblemAndRecovery(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		dir := t.TempDir()
		src := source{dir: dir}
		src.reload()
		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()
		changes := make(chan error)
		var frames []string
		done := make(chan error, 1)
		go func() {
			done <- show(ctx, io.Discard, func() (int, int, error) { return 160, 40, nil }, nil, nil, colorprofile.ASCII,
				func(sc Screen) Drawn {
					d := src.frame(sc)
					frames = append(frames, strings.Join(d.Lines, "\n"))
					return d
				}, options{changes: changes, reload: src.reload})
		}()
		check := func(want string) {
			t.Helper()
			synctest.Wait()
			if !strings.Contains(strings.Join(strings.Fields(strings.ReplaceAll(ansi.Strip(frames[len(frames)-1]), "▌", "")), " "), want) {
				t.Fatalf("live frame does not show %q:\n%s", want, frames[len(frames)-1])
			}
		}
		check("no session yet")
		path := filepath.Join(dir, "events.jsonl")
		if err := os.WriteFile(path, []byte("not json\n"), 0644); err != nil {
			t.Fatal(err)
		}
		changes <- nil
		check("invalid character")
		if err := os.WriteFile(path, watchSessionLine(t, "live journal recovered"), 0644); err != nil {
			t.Fatal(err)
		}
		changes <- nil
		check("live journal recovered")
		if diff := cmp.Diff(3, len(frames)); diff != "" {
			t.Fatalf("journal-driven frames (-want +got):\n%s", diff)
		}
		cancel()
		if err := <-done; err != nil {
			t.Fatal(err)
		}
	})
}

func TestHeldSourceStartsWithTheRunNotTheJournalItFound(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	path := filepath.Join(dir, "events.jsonl")
	src := source{dir: dir, waiting: true, since: 2}
	watchSourceFrame(t, &src, "STARTING", "NO SESSION YET")
	if err := os.WriteFile(path, watchSessionLine(t, "from an earlier run"), 0644); err != nil {
		t.Fatal(err)
	}
	watchSourceFrame(t, &src, "STARTING", "from an earlier run")
	grown := append(watchSessionLine(t, "from an earlier run"), watchWarningLine(t, "this run")...)
	if err := os.WriteFile(path, grown, 0644); err != nil {
		t.Fatal(err)
	}
	watchSourceFrame(t, &src, "this run", "STARTING")
}
