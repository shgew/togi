package main

import (
	"bytes"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"syscall"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/session"
)

var doctorBIOS = machine.BIOSContext{BIOSVersion: "3.14", Board: "ASRock X870E Taichi", CPUModel: "AMD Ryzen 9 9950X 16-Core Processor", Microcode: "0xb404038", BoostLimitMHz: 5750}

func doctorReadyChecks() []machine.Check {
	return []machine.Check{
		{Name: "cpu", Detail: "family 0x1A model 0x44 AMD Ryzen 9 9950X 16-Core Processor", OK: true},
		{Name: "ryzen_smu", Detail: "codename 23 (Granite Ridge), driver 0.7.1, SMU 98.92.0", OK: true},
		{Name: "pm_table", Detail: "pm_table version 0x620205: decoding per-core power, voltage request, temperature and C-state lanes", OK: true},
		{Name: "readback", Detail: "16 cores read back: 00:0 01:0 02:0 03:0 04:0 05:0 06:0 07:0 08:0 09:0 10:0 11:0 12:0 13:0 14:0 15:0", OK: true},
		{Name: "slot_mapping", Detail: "CCD0 fuse 0x00: cores 00-07 on slots 0-7; CCD1 fuse 0x00: cores 08-15 on slots 0-7", OK: true},
		{Name: "backends", Detail: "mprime: /nix/store/mprime/bin/mprime; y-cruncher: /nix/store/y-cruncher/bin/y-cruncher", OK: true},
		{Name: "backend_user", Detail: "togi-trial: uid 990 gid 990", OK: true},
		{Name: "systemd_run", Detail: "scope confined to cpu 0 created", OK: true},
		{Name: "watchdog", Detail: "no hardware watchdog state available"},
		{Name: "bios_context", Detail: "matches the session", OK: true},
	}
}

type fakeDiagnose struct {
	ran, skipped []machine.Check
	err          error
	calls        int
	recorded     *machine.BIOSContext
	privileged   bool
}

func (f *fakeDiagnose) diagnose(_ config.Config, recorded *machine.BIOSContext, privileged bool) ([]machine.Check, []machine.Check, error) {
	f.calls++
	f.recorded, f.privileged = recorded, privileged
	return f.ran, f.skipped, f.err
}

func linuxPlatform() error { return nil }

// doctorContextFixture is a journal stamped with ruleset whose session recorded doctorBIOS.
func doctorContextFixture(t *testing.T, ruleset int) string {
	t.Helper()
	dir := t.TempDir()
	data := currentJournalFixture(t, "testdata/events.jsonl")
	writeJournal(t, dir, bytes.Replace(data, []byte(`"ruleset":3`), fmt.Appendf(nil, `"ruleset":%d`, ruleset), 1))
	j, err := journal.Open(dir, journal.Options{Boot: "doctor-test"})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := j.Append(&journal.SessionContext{BIOSContext: doctorBIOS}); err != nil {
		t.Fatal(err)
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	return dir
}

func TestDoctorOutput(t *testing.T) {
	t.Parallel()
	ready := doctorReadyChecks()
	failed := doctorReadyChecks()
	failed[3] = machine.Check{Name: "readback", Detail: "core 00: read core 00 offset: RSMU command 0xd5: status 0xfc (rejected: busy)"}
	failed[7] = machine.Check{Name: "systemd_run", Detail: "systemd-run: exit status 1: Failed to start transient scope unit:\tAccess denied"}
	failed = failed[:len(failed)-1]
	changed := doctorReadyChecks()
	changed[9] = machine.Check{Name: "bios_context", Detail: "bios_version is 3.15; the session recorded 3.14; run archives this session and starts a new one"}
	for _, tc := range []struct {
		name         string
		privileged   bool
		ran, skipped []machine.Check
		code         int
	}{
		{"doctor-ready", true, ready, nil, exitOK},
		{"doctor-unprivileged", false,
			[]machine.Check{ready[0], ready[1], ready[5], ready[6], ready[8]},
			[]machine.Check{{Name: "pm_table", Detail: "needs root"}, {Name: "readback", Detail: "needs root"}, {Name: "slot_mapping", Detail: "needs root"}, {Name: "systemd_run", Detail: "needs root"}, {Name: "bios_context", Detail: "needs root"}},
			exitOK},
		{"doctor-failed", true, failed, []machine.Check{{Name: "bios_context", Detail: "needs every other check to pass"}}, exitPreflight},
		{"doctor-context-changed", true, changed, nil, exitOK},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			g := testGlobals(t)
			fake := &fakeDiagnose{ran: tc.ran, skipped: tc.skipped}
			var stdout, stderr bytes.Buffer
			if diff := cmp.Diff(tc.code, doctor(&g, nil, &stdout, &stderr, tc.privileged, linuxPlatform, fake.diagnose)); diff != "" {
				t.Fatalf("exit (-want +got): %s; stderr %s", diff, stderr.String())
			}
			golden(t, tc.name, stdout.String())
			wantStderr := ""
			if tc.code == exitPreflight {
				wantStderr = "togi doctor: not ready: readback, systemd_run\n"
			}
			if diff := cmp.Diff(wantStderr, stderr.String()); diff != "" {
				t.Fatalf("stderr (-want +got): %s", diff)
			}
		})
	}
}

func TestDoctorExitCodes(t *testing.T) {
	ok := []machine.Check{{Name: "cpu", OK: true}, {Name: "watchdog", Detail: "no hardware watchdog state available"}}
	changedContext := append(slices.Clone(ok), machine.Check{Name: "bios_context", Detail: "bios_version is 3.15; the session recorded 3.14; run archives this session and starts a new one"})
	for _, tc := range []struct {
		name       string
		privileged bool
		state      func(t *testing.T, g *globals)
		args       []string
		platform   error
		fake       fakeDiagnose
		code       int
		diagnosed  bool
		stderr     string
	}{
		{name: "ready with a watchdog warning", privileged: true, fake: fakeDiagnose{ran: ok}, code: exitOK, diagnosed: true},
		{name: "unprivileged without failure", fake: fakeDiagnose{ran: ok, skipped: []machine.Check{{Name: "readback", Detail: "needs root"}}}, code: exitOK, diagnosed: true},
		{name: "unprivileged failure", fake: fakeDiagnose{ran: []machine.Check{{Name: "backends", Detail: "backends.mprime not configured"}}}, code: exitPreflight, diagnosed: true, stderr: "not ready: backends"},
		{name: "privileged failure", privileged: true, fake: fakeDiagnose{ran: []machine.Check{{Name: "slot_mapping", Detail: "CCD0 fuse reads disagree"}}}, code: exitPreflight, diagnosed: true, stderr: "not ready: slot_mapping"},
		{name: "changed BIOS context warns", privileged: true, fake: fakeDiagnose{ran: changedContext}, code: exitOK, diagnosed: true},
		{name: "unreadable BIOS context", privileged: true, fake: fakeDiagnose{err: errors.New("read BIOS context: read /sys/class/dmi/id/bios_version: no such file or directory")}, code: exitError, diagnosed: true, stderr: "togi doctor: read BIOS context: read /sys/class/dmi/id/bios_version"},
		{name: "unsupported platform", privileged: true, platform: errors.New("hardware runs need Linux: unsupported operation"), fake: fakeDiagnose{ran: ok}, code: exitError, stderr: "togi doctor: hardware runs need Linux: unsupported operation"},
		{name: "unsupported platform unprivileged", platform: errors.New("hardware runs need Linux: unsupported operation"), fake: fakeDiagnose{ran: ok}, code: exitError, stderr: "togi doctor: hardware runs need Linux: unsupported operation"},
		{name: "host lock held", privileged: true, state: holdHostLock, fake: fakeDiagnose{ran: ok}, code: exitLocked, stderr: "another togi process holds the host lock"},
		{name: "unprivileged ignores host lock", state: holdHostLock, fake: fakeDiagnose{ran: ok}, code: exitOK, diagnosed: true},
		{name: "host lock unavailable", privileged: true, state: func(t *testing.T, g *globals) {
			t.Helper()
			g.hostLockPath = filepath.Join(t.TempDir(), "missing", "togi.lock")
		}, fake: fakeDiagnose{ran: ok}, code: exitError, stderr: filepath.Join("missing", "togi.lock")},
		{name: "missing explicit config", args: []string{"--config", "/nonexistent/togi.json"}, fake: fakeDiagnose{ran: ok}, code: exitUsage, stderr: "/nonexistent/togi.json"},
		{name: "newer schema", privileged: true, state: func(t *testing.T, g *globals) { t.Helper(); g.stateDir, _ = incompatibleFixture(t, "schema") }, fake: fakeDiagnose{ran: ok}, code: exitIncompatible, stderr: "this journal was written by"},
		{name: "newer ruleset", state: func(t *testing.T, g *globals) { t.Helper(); g.stateDir, _ = incompatibleFixture(t, "ruleset") }, fake: fakeDiagnose{ran: ok}, code: exitIncompatible, stderr: "ruleset 99"},
		{name: "unknown kinds", privileged: true, state: func(t *testing.T, g *globals) { t.Helper(); currentRulesetJournal(t, g, true) }, fake: fakeDiagnose{ran: ok}, code: exitIncompatible, stderr: `unknown kind "future.fact"`},
		{name: "unreadable journal", privileged: true, state: func(t *testing.T, g *globals) { t.Helper(); writeJournal(t, g.stateDir, []byte("not json\n")) }, fake: fakeDiagnose{ran: ok}, code: exitError, stderr: "read session.start stamp"},
		{name: "malformed event after current stamp", privileged: true, state: func(t *testing.T, g *globals) {
			t.Helper()
			currentRulesetJournal(t, g, false)
			appendJournal(t, g.stateDir, []byte("{\"kind\":\"trial.end\",\"data\":\n"))
		}, fake: fakeDiagnose{ran: ok}, code: exitError, stderr: "events.jsonl: journal line"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			g := testGlobals(t)
			if tc.state != nil {
				tc.state(t, &g)
			}
			before := directoryFiles(t, g.stateDir)
			var stdout, stderr bytes.Buffer
			platform := func() error { return tc.platform }
			code := doctor(&g, tc.args, &stdout, &stderr, tc.privileged, platform, tc.fake.diagnose)
			if diff := cmp.Diff(tc.code, code); diff != "" {
				t.Fatalf("exit (-want +got): %s; stdout %s; stderr %s", diff, stdout.String(), stderr.String())
			}
			if diff := cmp.Diff(tc.diagnosed, tc.fake.calls == 1); diff != "" {
				t.Fatalf("diagnosed (-want +got): %s", diff)
			}
			if tc.diagnosed && tc.fake.privileged != tc.privileged {
				t.Fatalf("diagnosed privileged=%v, want %v", tc.fake.privileged, tc.privileged)
			}
			if !strings.Contains(stderr.String(), tc.stderr) || (tc.stderr == "") != (stderr.Len() == 0) {
				t.Fatalf("stderr %q, want it to contain %q", stderr.String(), tc.stderr)
			}
			if diff := cmp.Diff(before, directoryFiles(t, g.stateDir)); diff != "" {
				t.Fatalf("doctor changed the state directory (-want +got): %s", diff)
			}
			if tc.platform != nil {
				if _, err := os.Stat(g.hostLockPath); !errors.Is(err, fs.ErrNotExist) {
					t.Fatalf("refused platform touched the host lock: %v", err)
				}
				if stdout.Len() != 0 {
					t.Fatalf("refused platform printed %q", stdout.String())
				}
			}
		})
	}
}

func holdHostLock(t *testing.T, g *globals) {
	t.Helper()
	lock, err := os.OpenFile(g.hostLockPath, os.O_CREATE|os.O_RDWR, 0o644)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = lock.Close() })
	if err := syscall.Flock(int(lock.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		t.Fatal(err)
	}
}

// currentRulesetJournal installs the journal fixture stamped with this build's ruleset, optionally with a future kind.
func currentRulesetJournal(t *testing.T, g *globals, future bool) {
	t.Helper()
	data := currentJournalFixture(t, "testdata/events.jsonl")
	if future {
		line, err := os.ReadFile("testdata/future-kind.jsonl")
		if err != nil {
			t.Fatal(err)
		}
		data = append(data, line...)
	}
	writeJournal(t, g.stateDir, bytes.Replace(data, []byte(`"ruleset":3`), fmt.Appendf(nil, `"ruleset":%d`, session.Build().Ruleset), 1))
}

func writeJournal(t *testing.T, dir string, data []byte) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), data, 0o644); err != nil {
		t.Fatal(err)
	}
}

func appendJournal(t *testing.T, dir string, data []byte) {
	t.Helper()
	f, err := os.OpenFile(filepath.Join(dir, "events.jsonl"), os.O_APPEND|os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := f.Write(data); err != nil {
		t.Fatal(err)
	}
	if err := f.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestDoctorPassesRecordedContext(t *testing.T) {
	for _, tc := range []struct {
		name    string
		journal bool
		context bool
		ruleset int
		want    *machine.BIOSContext
	}{
		{name: "no journal"},
		{name: "session without context", journal: true},
		{name: "recorded context", journal: true, context: true, ruleset: session.Build().Ruleset, want: &doctorBIOS},
		{name: "older journal run would archive", journal: true, context: true, ruleset: 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			g := testGlobals(t)
			switch {
			case tc.context:
				g.stateDir = doctorContextFixture(t, tc.ruleset)
			case tc.journal:
				currentRulesetJournal(t, &g, false)
			}
			fake := &fakeDiagnose{ran: []machine.Check{{Name: "cpu", OK: true}}}
			var stdout, stderr bytes.Buffer
			if code := doctor(&g, nil, &stdout, &stderr, true, linuxPlatform, fake.diagnose); code != exitOK {
				t.Fatalf("exit %d; stderr %s", code, stderr.String())
			}
			if diff := cmp.Diff(tc.want, fake.recorded); diff != "" {
				t.Fatalf("recorded context (-want +got):\n%s", diff)
			}
		})
	}
}

func TestDoctorNotReadyStyle(t *testing.T) {
	for _, tc := range []struct {
		noColor string
		want    string
	}{
		{"", "<3>\x1b[1;31mtogi doctor: not ready: readback\x1b[0m\n"},
		{"1", "<3>togi doctor: not ready: readback\n"},
	} {
		t.Run("NO_COLOR="+tc.noColor, func(t *testing.T) {
			t.Setenv("NO_COLOR", tc.noColor)
			stderr, err := os.CreateTemp(t.TempDir(), "doctor-stderr")
			if err != nil {
				t.Fatal(err)
			}
			defer stderr.Close()
			t.Setenv("JOURNAL_STREAM", journalStreamFor(t, stderr))
			g := testGlobals(t)
			fake := &fakeDiagnose{ran: []machine.Check{{Name: "readback", Detail: "core 00 refused"}}}
			var stdout bytes.Buffer
			if code := doctor(&g, nil, &stdout, stderr, true, linuxPlatform, fake.diagnose); code != exitPreflight {
				t.Fatalf("exit %d, want %d", code, exitPreflight)
			}
			got, err := os.ReadFile(stderr.Name())
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.want, string(got)); diff != "" {
				t.Fatalf("stderr (-want +got): %q", diff)
			}
			if strings.Contains(stdout.String(), "\x1b") {
				t.Fatalf("colored stdout: %q", stdout.String())
			}
		})
	}
}
