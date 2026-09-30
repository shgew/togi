package main

import (
	"bytes"
	"context"
	"errors"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"syscall"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
)

func TestResetRefusesHostLock(t *testing.T) {
	for _, args := range [][]string{{"--core", "3"}, {"--all"}} {
		t.Run(strings.Join(args, " "), func(t *testing.T) {
			dir := resetCandidateFixture(t, -10)
			marker := filepath.Join(dir, "archive", "session-carry-pending")
			if err := os.MkdirAll(filepath.Dir(marker), 0o755); err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(marker, nil, 0o644); err != nil {
				t.Fatal(err)
			}
			before := directoryFiles(t, dir)
			path := filepath.Join(t.TempDir(), "togi.lock")
			lock, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o644)
			if err != nil {
				t.Fatal(err)
			}
			t.Cleanup(func() { _ = lock.Close() })
			if err := syscall.Flock(int(lock.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
				t.Fatal(err)
			}
			var stdout, stderr bytes.Buffer
			g := &globals{config: filepath.Join(t.TempDir(), "config.toml"), stateDir: dir, hostLockPath: path}
			code := runReset(g, args, &stdout, &stderr)
			if diff := cmp.Diff(exitLocked, code); diff != "" {
				t.Errorf("contention exit (-want +got): %s; stderr %s", diff, stderr.String())
			}
			if !strings.Contains(stderr.String(), path) {
				t.Errorf("contention did not name lock %s: %s", path, stderr.String())
			}
			if diff := cmp.Diff(before, directoryFiles(t, dir)); diff != "" {
				t.Errorf("contended reset changed state (-want +got): %s", diff)
			}
		})
	}
}

func testGlobals(t *testing.T) globals {
	t.Helper()
	return globals{
		config:       filepath.Join(t.TempDir(), "config.toml"),
		stateDir:     t.TempDir(),
		hostLockPath: filepath.Join(t.TempDir(), "togi.lock"),
	}
}

func testCLI(t *testing.T, args []string, stdout, stderr io.Writer) int {
	t.Helper()
	return cliWithGlobals(args, stdout, stderr, testGlobals(t))
}

func TestRunRefusesHostLockBeforeHardwareAndCarry(t *testing.T) {
	if runtime.GOOS != "linux" {
		t.Skip("hardware runs need Linux")
	}
	g := testGlobals(t)
	g.stateDir = resetCandidateFixture(t, -10)
	before := directoryFiles(t, g.stateDir)
	lock, err := os.OpenFile(g.hostLockPath, os.O_CREATE|os.O_RDWR, 0o644)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = lock.Close() })
	if err := syscall.Flock(int(lock.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		t.Fatal(err)
	}
	constructed := false
	newMachine := func(config.Config, string) (machine.Machine, error) {
		constructed = true
		return machine.Machine{}, errors.New("hardware construction must not run")
	}
	var stderr bytes.Buffer
	code := runHardware(context.Background(), &g, config.Default(), false, nil, 0, &stderr, journal.Renderer{}, nil, newMachine)
	if diff := cmp.Diff(exitLocked, code); diff != "" {
		t.Errorf("contention exit (-want +got): %s; stderr %s", diff, stderr.String())
	}
	if constructed {
		t.Error("contended run constructed hardware")
	}
	if !strings.Contains(stderr.String(), g.hostLockPath) {
		t.Errorf("contention did not name lock %s: %s", g.hostLockPath, stderr.String())
	}
	if diff := cmp.Diff(before, directoryFiles(t, g.stateDir)); diff != "" {
		t.Errorf("contended run changed state (-want +got): %s", diff)
	}
}

type refusedStartupHost struct {
	machine.Host
	t *testing.T
}

func (h refusedStartupHost) BIOSContext() (machine.BIOSContext, error) {
	h.t.Fatal("BIOS access before supported identity")
	return machine.BIOSContext{}, nil
}

func TestRunRefusesIdentityBeforeBIOSOrSMUAccess(t *testing.T) {
	if runtime.GOOS != "linux" {
		t.Skip("hardware runs need Linux")
	}
	for _, detail := range []string{"unsupported CPU family", "unsupported CPU model", "unsupported driver codename"} {
		t.Run(detail, func(t *testing.T) {
			g := testGlobals(t)
			m, err := sim.New(sim.Config{Seed: 82})
			if err != nil {
				t.Fatal(err)
			}
			if detail == "unsupported driver codename" {
				m.FailCheck("ryzen_smu", detail)
			} else {
				m.FailCheck("cpu", detail)
			}
			seams := m.Seams()
			seams.Host = refusedStartupHost{Host: seams.Host, t: t}
			seams.SMU = nil
			newMachine := func(config.Config, string) (machine.Machine, error) {
				return seams, nil
			}
			bootloader := &clearingBootloader{}
			var stderr bytes.Buffer
			code := runHardware(context.Background(), &g, config.Default(), false, bootloader, 0, &stderr, journal.Renderer{}, nil, newMachine)
			if code != exitPreflight || !strings.Contains(stderr.String(), detail) {
				t.Fatalf("identity refusal: exit %d, stderr %s", code, stderr.String())
			}
			if bootloader.calls != 1 {
				t.Fatalf("saved entry cleared %d times, want once", bootloader.calls)
			}
			events, _, err := journal.Read(g.stateDir)
			if err != nil {
				t.Fatal(err)
			}
			var checks []journal.PreflightCheck
			var deadEnd *journal.DeadEnd
			var entry *journal.BootSavedEntry
			for _, e := range events {
				switch p := e.Data.(type) {
				case *journal.PreflightCheck:
					checks = append(checks, *p)
				case *journal.DeadEnd:
					deadEnd = p
				case *journal.BootSavedEntry:
					entry = p
				}
			}
			if len(checks) == 0 || checks[0].Check != "watchdog" || !checks[0].OK {
				t.Fatalf("first preflight check is not successful watchdog: %+v", checks)
			}
			if deadEnd == nil || deadEnd.Condition != journal.DeadEndPreflight || deadEnd.Action != journal.ActionClearSavedEntry {
				t.Fatalf("missing normal preflight refusal: %+v", deadEnd)
			}
			if entry == nil || entry.Before != "togi" || entry.After != "" || entry.Error != "" {
				t.Fatalf("missing saved-entry event: %+v", entry)
			}
			if events[len(events)-1].Kind != journal.KindShutdown {
				t.Fatal("preflight refusal did not finish cleanly")
			}
			t.Logf("exit %d; first preflight %s ok; deadend %s; saved entry %q -> %q; no BIOS/SMU accesses", code, checks[0].Check, deadEnd.Condition, entry.Before, entry.After)
		})
	}
}

func TestRunReportsConstructionError(t *testing.T) {
	if runtime.GOOS != "linux" {
		t.Skip("hardware runs need Linux")
	}
	g := testGlobals(t)
	newMachine := func(config.Config, string) (machine.Machine, error) {
		return machine.Machine{}, errors.New("read CPU topology: unavailable")
	}
	var stderr bytes.Buffer
	code := runHardware(context.Background(), &g, config.Default(), false, nil, 0, &stderr, journal.Renderer{}, nil, newMachine)
	if code != exitError || !strings.Contains(stderr.String(), "read CPU topology: unavailable") {
		t.Fatalf("construction error: exit %d, stderr %s", code, stderr.String())
	}
}

func TestReadOnlyCommandsDoNotTakeHostLock(t *testing.T) {
	for _, args := range [][]string{{"status"}, {"cert"}, {"events"}, {"watch", "--width", "120", "--height", "33"}} {
		t.Run(args[0], func(t *testing.T) {
			g := testGlobals(t)
			g.stateDir = resetCandidateFixture(t, -10)
			before := directoryFiles(t, g.stateDir)
			g.hostLockPath = filepath.Join(t.TempDir(), "missing", "togi.lock")
			var stdout, stderr bytes.Buffer
			code := cliWithGlobals(args, &stdout, &stderr, g)
			if diff := cmp.Diff(exitOK, code); diff != "" {
				t.Errorf("read-only exit (-want +got): %s; stderr %s", diff, stderr.String())
			}
			if _, err := os.Stat(g.hostLockPath); !errors.Is(err, fs.ErrNotExist) {
				t.Errorf("read-only command created host lock: %v", err)
			}
			if diff := cmp.Diff(before, directoryFiles(t, g.stateDir)); diff != "" {
				t.Errorf("read-only command changed state (-want +got): %s", diff)
			}
		})
	}
}

func directoryFiles(t *testing.T, dir string) map[string]string {
	t.Helper()
	files := make(map[string]string)
	err := filepath.WalkDir(dir, func(path string, entry fs.DirEntry, err error) error {
		if err != nil || entry.IsDir() {
			return err
		}
		data, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		rel, err := filepath.Rel(dir, path)
		if err != nil {
			return err
		}
		files[rel] = string(data)
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	return files
}
