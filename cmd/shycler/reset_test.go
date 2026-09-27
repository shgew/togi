package main

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/shycler/internal/journal"
	"github.com/shgew/shycler/internal/machine"
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
		edge    int
		warning bool
	}{
		{"at failed mark", 3, -10, true},
		{"deeper than failed mark", 3, -11, true},
		{"shallower than failed mark", 3, -9, false},
		{"no failed mark", 7, -10, false},
		{"suspect backoff only", 7, -11, false},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dir := resetCandidateFixture(t)
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
			if !strings.Contains(stdout.String(), "archived to archive/") {
				t.Errorf("missing archive line: %q", stdout.String())
			}
		})
	}
}

func resetCandidateFixture(t *testing.T) string {
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
	fail := appendEvent(&journal.Failure{Signal: machine.Signal("error"), Attribution: journal.Attributed, Core: new(3), Offset: new(-10)})
	appendEvent(&journal.TunerDecision{
		Core: 3, Phase: journal.PhaseSearch, Decision: journal.Backoff,
		FromOffset: -10, ToOffset: -9, FailedMark: new(-10), Reason: "attributed failure",
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
			dir := resetCandidateFixture(t)
			cfg := filepath.Join(t.TempDir(), "config.toml")
			if tt.config != "" {
				if err := os.WriteFile(cfg, []byte(tt.config), 0o644); err != nil {
					t.Fatal(err)
				}
			}
			var stdout, stderr bytes.Buffer
			args := []string{"--state-dir", dir, "reset", "--all"}
			if tt.explicit {
				args = append(args, "--config", cfg)
			} else {
				g := &globals{config: cfg, stateDir: dir}
				if code := runReset(g, []string{"--all"}, &stdout, &stderr); code != exitOK {
					t.Fatalf("reset exit %d: %s", code, stderr.String())
				}
			}
			if tt.explicit {
				if code := cli(args, &stdout, &stderr); code != exitOK {
					t.Fatalf("reset exit %d: %s", code, stderr.String())
				}
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

func TestResetAllSkipsIncompatibleRulesetCandidateCheck(t *testing.T) {
	dir, _ := incompatibleFixture(t, "ruleset")
	cfg := filepath.Join(t.TempDir(), "config.toml")
	if err := os.WriteFile(cfg, []byte("[candidate_edges]\n\"3\" = -50\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	var stdout, stderr bytes.Buffer
	if code := cli([]string{"--state-dir", dir, "--config", cfg, "reset", "--all"}, &stdout, &stderr); code != exitOK {
		t.Fatalf("reset exit %d: %s", code, stderr.String())
	}
	if strings.Contains(stderr.String(), "candidate edge") || !strings.Contains(stdout.String(), "archived to archive/") {
		t.Errorf("incompatible archive: stdout %q, stderr %q", stdout.String(), stderr.String())
	}
}
