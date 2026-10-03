package main

import (
	"bytes"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/session"
)

func TestCommandsRefuseWhileLocked(t *testing.T) {
	fixture, err := os.ReadFile("testdata/events.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	fixture = bytes.Replace(fixture, []byte(`"schema":2,"ruleset":3`), fmt.Appendf(nil, `"schema":2,"ruleset":%d`, session.Build().Ruleset), 1)
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), fixture, 0o644); err != nil {
		t.Fatal(err)
	}
	j, err := journal.Open(dir, journal.Options{Boot: "run"})
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	for _, args := range [][]string{{"reset", "--core", "3"}, {"reset", "--all"}} {
		var stdout, stderr bytes.Buffer
		if code := testCLI(t, append([]string{"--state-dir", dir}, args...), &stdout, &stderr); code != exitLocked {
			t.Errorf("%v: exit %d, want %d; stderr %q", args, code, exitLocked, stderr.String())
		}
	}
	for _, args := range [][]string{{"reset"}, {"reset", "--core", "3", "--all"}} {
		var stdout, stderr bytes.Buffer
		if code := testCLI(t, append([]string{"--state-dir", dir}, args...), &stdout, &stderr); code != exitUsage {
			t.Errorf("%v: exit %d, want %d", args, code, exitUsage)
		}
	}
	after, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
	if err != nil || !bytes.Equal(after, fixture) {
		t.Fatalf("journal changed while locked: %v", err)
	}
}

func TestResetAllCandidateEdges(t *testing.T) {
	tests := []struct {
		name    string
		core    int
		mark    int
		edge    int
		warning bool
	}{
		{"at failed mark", 3, -10, -10, true},
		{"deeper than failed mark", 3, -10, -11, true},
		{"shallower than failed mark", 3, -10, -9, false},
		{"failed at zero", 3, 0, 0, true},
		{"no failed mark", 7, -10, -10, false},
		{"suspect backoff only", 7, -10, -11, false},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dir := resetCandidateFixture(t, tt.mark)
			cfg := filepath.Join(t.TempDir(), "config.toml")
			if err := os.WriteFile(cfg, []byte(fmt.Sprintf("[candidate_edges]\n\"%d\" = %d\n", tt.core, tt.edge)), 0o644); err != nil {
				t.Fatal(err)
			}
			var stdout, stderr bytes.Buffer
			if code := testCLI(t, []string{"reset", "--all", "--state-dir", dir, "--config", cfg}, &stdout, &stderr); code != exitOK {
				t.Fatalf("reset exit %d: %s", code, stderr.String())
			}
			got := strings.Contains(stderr.String(), fmt.Sprintf("candidate edge %d for core %02d", tt.edge, tt.core))
			if diff := cmp.Diff(tt.warning, got); diff != "" {
				t.Errorf("candidate warning (-want +got):\n%s; stderr %q", diff, stderr.String())
			}
			if tt.warning {
				remedy := "remove it"
				if tt.mark < 0 {
					remedy = fmt.Sprintf("use %d, the failed mark plus one, or remove it", tt.mark+1)
				}
				if !strings.Contains(stderr.String(), remedy) {
					t.Errorf("missing remedy %q: %q", remedy, stderr.String())
				}
			}
			if !strings.Contains(stdout.String(), "archived to archive/") {
				t.Errorf("missing archive line: %q", stdout.String())
			}
		})
	}
}

func resetCandidateFixture(t *testing.T, mark int) string {
	t.Helper()
	dir := t.TempDir()
	installJournalFixture(t, dir)
	j, err := journal.Open(dir, journal.Options{Boot: "reset-test"})
	if err != nil {
		t.Fatal(err)
	}
	appendEvent := func(p journal.Payload, cause ...int) journal.Event {
		t.Helper()
		event, err := j.Append(p, cause...)
		if err != nil {
			t.Fatal(err)
		}
		return event
	}
	fail := appendEvent(&journal.Failure{Signal: machine.Signal("error"), Attribution: journal.Attributed, Core: new(3), Offset: new(mark)})
	appendEvent(&journal.TunerDecision{
		Core: 3, Phase: journal.PhaseSearch, Decision: journal.Backoff,
		FromOffset: mark, ToOffset: min(mark+1, 0), FailedMark: new(mark), Reason: "attributed failure",
	}, fail.Seq)
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	return dir
}

func TestResetAllSkipsUnavailableConfig(t *testing.T) {
	for _, tt := range []struct {
		name, config string
		explicit     bool
		warning      bool
	}{
		{"default missing", "", false, false},
		{"explicit missing", "", true, true},
		{"default invalid", "removed_key = true\n", false, false},
		{"explicit invalid", "removed_key = true\n", true, true},
	} {
		t.Run(tt.name, func(t *testing.T) {
			dir := resetCandidateFixture(t, -10)
			cfg := filepath.Join(t.TempDir(), "config.toml")
			if tt.config != "" {
				if err := os.WriteFile(cfg, []byte(tt.config), 0o644); err != nil {
					t.Fatal(err)
				}
			}
			var stdout, stderr bytes.Buffer
			g := &globals{config: cfg, stateDir: dir, configSet: tt.explicit, hostLockPath: filepath.Join(t.TempDir(), "togi.lock")}
			if code := runReset(g, []string{"--all"}, &stdout, &stderr); code != exitOK {
				t.Fatalf("reset exit %d: %s", code, stderr.String())
			}
			if diff := cmp.Diff(tt.warning, strings.Contains(stderr.String(), "cannot check candidate edges")); diff != "" {
				t.Errorf("config warning (-want +got):\n%s; stderr %q", diff, stderr.String())
			}
			if !strings.Contains(stdout.String(), "archived to archive/") {
				t.Errorf("missing archive line: %q", stdout.String())
			}
		})
	}
}

func TestResetAllDropsAPendingCarry(t *testing.T) {
	for _, tc := range []struct {
		name    string
		journal bool
		locked  bool
		code    int
		dropped bool
	}{
		{"with a journal", true, false, exitOK, true},
		{"before the new session's journal", false, false, exitOK, true},
		{"while a run holds the lock", false, true, exitLocked, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			marker := filepath.Join(dir, "archive", "x-carry-pending")
			if err := os.MkdirAll(filepath.Dir(marker), 0o755); err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(marker, nil, 0o644); err != nil {
				t.Fatal(err)
			}
			if tc.journal {
				installJournalFixture(t, dir)
			}
			if tc.locked {
				j, err := journal.Open(dir, journal.Options{Boot: "run"})
				if err != nil {
					t.Fatal(err)
				}
				defer j.Close()
			}
			var stdout, stderr bytes.Buffer
			g := &globals{config: filepath.Join(t.TempDir(), "config.toml"), stateDir: dir, hostLockPath: filepath.Join(t.TempDir(), "togi.lock")}
			if code := runReset(g, []string{"--all"}, &stdout, &stderr); code != tc.code {
				t.Fatalf("reset exit %d, want %d: %s", code, tc.code, stderr.String())
			}
			if _, err := os.Stat(marker); errors.Is(err, fs.ErrNotExist) != tc.dropped {
				t.Fatalf("carry marker after reset --all: %v, want dropped %v", err, tc.dropped)
			}
		})
	}
}

func TestResetAllWarnsAcrossRulesets(t *testing.T) {
	dir := resetCandidateFixture(t, -10)
	path := filepath.Join(dir, "events.jsonl")
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	older := strings.ReplaceAll(string(data), `"ruleset":3`, `"ruleset":2`)
	if older == string(data) {
		t.Fatal("fixture did not contain a ruleset stamp")
	}
	if err := os.WriteFile(path, []byte(older), 0o644); err != nil {
		t.Fatal(err)
	}
	cfg := filepath.Join(t.TempDir(), "config.toml")
	if err := os.WriteFile(cfg, []byte("[candidate_edges]\n\"3\" = -10\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	var stdout, stderr bytes.Buffer
	if code := testCLI(t, []string{"--state-dir", dir, "--config", cfg, "reset", "--all"}, &stdout, &stderr); code != exitOK {
		t.Fatalf("reset exit %d: %s", code, stderr.String())
	}
	if !strings.Contains(stderr.String(), "candidate edge -10 for core 03") || !strings.Contains(stdout.String(), "archived to archive/") {
		t.Errorf("ruleset-2 archive: stdout %q, stderr %q", stdout.String(), stderr.String())
	}
}

func TestResetAllCannotInterruptLockedTransition(t *testing.T) {
	dir := resetCandidateFixture(t, -10)
	marker := filepath.Join(dir, "archive", "original-carry-pending")
	if err := os.MkdirAll(filepath.Dir(marker), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(marker, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	before, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	locked, release, done := make(chan struct{}), make(chan struct{}), make(chan error, 1)
	go func() {
		j, err := journal.Lock(dir, journal.Options{})
		if err != nil {
			done <- err
			close(locked)
			return
		}
		close(locked)
		<-release
		done <- j.Close()
	}()
	<-locked
	var stdout, stderr bytes.Buffer
	g := &globals{stateDir: dir, hostLockPath: filepath.Join(t.TempDir(), "host.lock")}
	code := runReset(g, []string{"--all"}, &stdout, &stderr)
	close(release)
	lockErr := <-done
	if lockErr != nil {
		t.Fatal(lockErr)
	}
	if code != exitLocked {
		t.Fatalf("reset exit %d: %s", code, stderr.String())
	}
	after, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
	if err != nil || !bytes.Equal(before, after) {
		t.Fatalf("reset changed winning transition journal: %v", err)
	}
	if _, err := os.Stat(marker); err != nil {
		t.Fatalf("reset removed winning transition marker: %v", err)
	}
}

func TestMissingSessionRefusals(t *testing.T) {
	t.Parallel()
	for _, empty := range []bool{false, true} {
		for _, command := range []string{"status", "reset"} {
			t.Run(fmt.Sprintf("%s/empty=%t", command, empty), func(t *testing.T) {
				g := testGlobals(t)
				if empty {
					if err := os.WriteFile(filepath.Join(g.stateDir, "events.jsonl"), nil, 0o600); err != nil {
						t.Fatal(err)
					}
				}
				before := directoryFiles(t, g.stateDir)
				var out, diagnostics bytes.Buffer
				args := []string{command}
				if command == "reset" {
					args = append(args, "--all")
				}
				code := cliWithGlobals(args, &out, &diagnostics, g)
				want := fmt.Sprintf("togi %s: no journal at %s\n", command, filepath.Join(g.stateDir, "events.jsonl"))
				if empty {
					want = fmt.Sprintf("togi %s: no session in %s\n", command, g.stateDir)
				}
				if code != exitError || out.Len() != 0 {
					t.Fatalf("exit %d, stdout %q", code, out.String())
				}
				if diff := cmp.Diff(want, diagnostics.String()); diff != "" {
					t.Fatalf("refusal: %s", diff)
				}
				if diff := cmp.Diff(before, directoryFiles(t, g.stateDir)); diff != "" {
					t.Fatalf("refusal changed state: %s", diff)
				}
			})
		}
	}
}

func TestResetCoreCommandOutcome(t *testing.T) {
	t.Parallel()
	for _, core := range []int{3, 999} {
		t.Run(fmt.Sprint(core), func(t *testing.T) {
			dir := t.TempDir()
			fixture, err := os.ReadFile("testdata/events.jsonl")
			if err != nil {
				t.Fatal(err)
			}
			fixture = bytes.Replace(fixture, []byte(`"schema":2,"ruleset":3`), fmt.Appendf(nil, `"schema":2,"ruleset":%d`, session.Build().Ruleset), 1)
			if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), fixture, 0o600); err != nil {
				t.Fatal(err)
			}
			j, err := journal.Lock(dir, journal.Options{Boot: "reset-boot", Now: func() time.Time { return time.Unix(100, 0).UTC() }})
			if err != nil {
				t.Fatal(err)
			}
			defer j.Close()
			var diagnostics bytes.Buffer
			_, code, ok := openForCommand("reset", j, &diagnostics, true)
			if !ok || code != exitOK {
				t.Fatalf("open exit %d: %s", code, diagnostics.String())
			}
			code, ok = closeCommand("reset", j, session.ResetCore(j, core), &diagnostics)
			if core == 999 {
				if code != exitUsage || ok || !strings.Contains(diagnostics.String(), "core 999") {
					t.Fatalf("exit %d, ok %t: %s", code, ok, diagnostics.String())
				}
				return
			}
			if code != exitOK || !ok {
				t.Fatalf("exit %d: %s", code, diagnostics.String())
			}
			events, _, err := journal.Read(dir)
			if err != nil {
				t.Fatal(err)
			}
			p, ok := events[len(events)-2].Data.(*journal.CommandReset)
			if !ok || p.Core == nil || *p.Core != 3 || p.All {
				t.Fatalf("missing queued core reset: %+v", events[len(events)-2])
			}
			if events[len(events)-1].Kind != journal.KindShutdown {
				t.Fatal("reset did not finish with a command shutdown")
			}
		})
	}
}

func TestResetErrorEscapesDiagnostic(t *testing.T) {
	t.Parallel()
	var out bytes.Buffer
	if code := resetError(errors.New("read failed\x1b[2J\nforged"), journal.ErrLocked, &out); code != exitError {
		t.Fatalf("exit %d", code)
	}
	if diff := cmp.Diff("togi reset: read failed\\x1b[2J\\nforged\n", out.String()); diff != "" {
		t.Fatalf("reset diagnostic (-want +got): %s", diff)
	}
}
