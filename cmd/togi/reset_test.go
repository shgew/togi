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

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestCommandsRefuseWhileLocked(t *testing.T) {
	fixture, err := os.ReadFile("testdata/events.jsonl")
	if err != nil {
		t.Fatal(err)
	}
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
		if code := cli(append([]string{"--state-dir", dir}, args...), &stdout, &stderr); code != exitLocked {
			t.Errorf("%v: exit %d, want %d; stderr %q", args, code, exitLocked, stderr.String())
		}
	}
	for _, args := range [][]string{{"reset"}, {"reset", "--core", "3", "--all"}} {
		var stdout, stderr bytes.Buffer
		if code := cli(append([]string{"--state-dir", dir}, args...), &stdout, &stderr); code != exitUsage {
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
			if code := cli([]string{"reset", "--all", "--state-dir", dir, "--config", cfg}, &stdout, &stderr); code != exitOK {
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
	fixture, err := os.ReadFile("testdata/events.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), fixture, 0o644); err != nil {
		t.Fatal(err)
	}
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
	appendEvent(&journal.CorePhase{Core: 7, To: journal.PhaseGuard, Offset: -10, Reason: "confirmed"})
	appendEvent(&journal.TunerDecision{
		Core: 7, Phase: journal.PhaseGuard, Decision: journal.SuspectBackoff,
		FromOffset: -10, ToOffset: -9, UnprovenDepth: 1, Reason: "unattributed failure",
	})
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
			g := &globals{config: cfg, stateDir: dir, configSet: tt.explicit}
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
	fixture, err := os.ReadFile("testdata/events.jsonl")
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	marker := filepath.Join(dir, "archive", "x-carry-pending")
	if err := os.MkdirAll(filepath.Dir(marker), 0o755); err != nil {
		t.Fatal(err)
	}
	for path, data := range map[string][]byte{filepath.Join(dir, "events.jsonl"): fixture, marker: nil} {
		if err := os.WriteFile(path, data, 0o644); err != nil {
			t.Fatal(err)
		}
	}
	var stdout, stderr bytes.Buffer
	g := &globals{config: filepath.Join(t.TempDir(), "config.toml"), stateDir: dir}
	if code := runReset(g, []string{"--all"}, &stdout, &stderr); code != exitOK {
		t.Fatalf("reset exit %d: %s", code, stderr.String())
	}
	if _, err := os.Stat(marker); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("carry marker after reset --all: %v", err)
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
	if code := cli([]string{"--state-dir", dir, "--config", cfg, "reset", "--all"}, &stdout, &stderr); code != exitOK {
		t.Fatalf("reset exit %d: %s", code, stderr.String())
	}
	if !strings.Contains(stderr.String(), "candidate edge -10 for core 03") || !strings.Contains(stdout.String(), "archived to archive/") {
		t.Errorf("ruleset-2 archive: stdout %q, stderr %q", stdout.String(), stderr.String())
	}
}
