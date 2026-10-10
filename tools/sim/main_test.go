package main

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"

	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/simrun"
)

func TestSimRefusesInvalidInputs(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name       string
		args       []string
		code       int
		diagnostic string
	}{
		{"unknown flag", []string{"--unknown"}, 2, "flag provided but not defined: -unknown"},
		{"positional argument", []string{"extra"}, 2, "sim: unexpected positional arguments"},
		{"zero cycles", []string{"--cycles", "0"}, 2, "sim: --cycles must be a positive integer"},
		{"negative cycles", []string{"--cycles", "-1"}, 2, "sim: --cycles must be a positive integer"},
		{"zero boots", []string{"--max-boots", "0"}, 2, "sim: --max-boots must be a positive integer"},
		{"negative verify interval", []string{"--verify-every", "-1"}, 2, "sim: --verify-every must not be negative"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var out bytes.Buffer
			if got := run(tc.args, &out); got != tc.code || !strings.Contains(out.String(), tc.diagnostic) {
				t.Fatalf("exit %d, output %q; want %d, %q", got, out.String(), tc.code, tc.diagnostic)
			}
		})
	}
}

func TestSimBootCapExitsCensored(t *testing.T) {
	t.Parallel()
	var out bytes.Buffer
	got := run([]string{"--max-boots", "2", "--state-dir", t.TempDir()}, &out)
	if got != 3 || !strings.Contains(out.String(), "sim: simulate session: simulated machine reached its boot cap without stopping after 2 boots") {
		t.Fatalf("exit %d, output %q", got, out.String())
	}
}

func TestSimInvalidMachineLeavesStateUntouched(t *testing.T) {
	t.Parallel()
	root := t.TempDir()
	path := filepath.Join(root, "machine.toml")
	if err := os.WriteFile(path, []byte("unknown_machine_key = 1\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	state := filepath.Join(root, "state")
	var out bytes.Buffer
	if got := run([]string{"--machine", path, "--state-dir", state}, &out); got != 1 || !strings.Contains(out.String(), "sim: load machine:") {
		t.Fatalf("exit %d, output %q", got, out.String())
	}
	if _, err := os.Stat(state); !os.IsNotExist(err) {
		t.Fatalf("invalid machine changed state directory: %v", err)
	}
}

func TestSimReplayRequiresMachineBeforeStateChanges(t *testing.T) {
	t.Parallel()
	state := filepath.Join(t.TempDir(), "state")
	var out bytes.Buffer
	if got := run([]string{"--replay-facts", "--state-dir", state}, &out); got != 1 || out.String() != "sim: load replay oracle: machine has no facts extract\n" {
		t.Fatalf("exit %d, output %q", got, out.String())
	}
	if _, err := os.Stat(state); !os.IsNotExist(err) {
		t.Fatalf("invalid replay changed state directory: %v", err)
	}
}

func TestSimInvalidMachineCreatesNoTemporaryState(t *testing.T) {
	tmp := t.TempDir()
	t.Setenv("TMPDIR", tmp)
	machine := filepath.Join(t.TempDir(), "machine.toml")
	if err := os.WriteFile(machine, []byte("unknown_machine_key = 1\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	for _, args := range [][]string{{"--machine", machine}, {"--replay-facts"}} {
		var out bytes.Buffer
		if got := run(args, &out); got != 1 || strings.Contains(out.String(), "state directory") {
			t.Fatalf("%v: exit %d, output %q", args, got, out.String())
		}
	}
	entries, err := os.ReadDir(tmp)
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 0 {
		t.Fatalf("invalid input created temporary state: %v", entries)
	}
}

func TestSimDefaultStateDirIsAnnouncedTemporaryDirectory(t *testing.T) {
	tmp := t.TempDir()
	t.Setenv("TMPDIR", tmp)
	var out bytes.Buffer
	if got := run([]string{"--max-boots", "2"}, &out); got != 3 {
		t.Fatalf("exit %d, output %q", got, out.String())
	}
	entries, err := os.ReadDir(tmp)
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 1 || !strings.HasPrefix(entries[0].Name(), "togi-sim-") {
		t.Fatalf("temporary directory entries: %v", entries)
	}
	dir := filepath.Join(tmp, entries[0].Name())
	if line, _, _ := strings.Cut(out.String(), "\n"); line != "sim: state directory "+dir {
		t.Fatalf("first output line %q, want the state directory %s", line, dir)
	}
	events, torn, err := journal.Read(dir)
	if err != nil || torn != nil {
		t.Fatalf("read journal: %v, torn %q", err, torn)
	}
	boots := map[string]bool{}
	for _, e := range events {
		boots[e.Boot] = true
	}
	if diff := cmp.Diff(2, len(boots)); diff != "" {
		t.Fatalf("boots journaled in the state directory (-want +got):\n%s", diff)
	}
}

func TestSimSamplesAreOptIn(t *testing.T) {
	t.Parallel()
	for _, writeSamples := range []bool{false, true} {
		name := "in-memory"
		if writeSamples {
			name = "on-disk"
		}
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			dir := t.TempDir()
			args := []string{"--state-dir", dir}
			if writeSamples {
				args = append(args, "--samples")
			}
			if diff := cmp.Diff(0, run(args, io.Discard)); diff != "" {
				t.Fatalf("exit code (-want +got):\n%s", diff)
			}
			files, err := filepath.Glob(filepath.Join(dir, "trials", "*", "samples.jsonl"))
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(writeSamples, len(files) > 0); diff != "" {
				t.Fatalf("sample files exist (-want +got):\n%s", diff)
			}
		})
	}
}

func TestSimResumesUnderTheRecordedConfiguration(t *testing.T) {
	t.Parallel()
	recorded := config.Default()
	recorded.Backends = config.Backends{Mprime: "/nix/store/recorded-mprime", Ycruncher: "/nix/store/recorded-ycruncher"}
	recorded.Durations.SearchTrialS = 17
	dir := recordTwoTrials(t, recorded)
	if got := run([]string{"--state-dir", dir, "--max-boots", "1"}, io.Discard); got != 0 && got != 3 {
		t.Fatalf("exit %d", got)
	}
	events, _, err := journal.Read(dir)
	if err != nil {
		t.Fatal(err)
	}
	var loaded []journal.ConfigSnapshot
	for _, e := range events {
		if p, ok := e.Data.(*journal.ConfigLoaded); ok {
			loaded = append(loaded, p.Config)
		}
	}
	if len(loaded) < 2 {
		t.Fatalf("%d config.loaded events, want the original and the resume", len(loaded))
	}
	if diff := cmp.Diff(loaded[0], loaded[len(loaded)-1]); diff != "" {
		t.Fatalf("configuration the resume recorded (-original +resumed):\n%s", diff)
	}
}

func TestSimTransitionsAnOlderSchemaUnderTheRecordedBackends(t *testing.T) {
	t.Parallel()
	recorded := config.Default()
	recorded.Backends = config.Backends{Mprime: "/nix/store/recorded-mprime", Ycruncher: "/nix/store/recorded-ycruncher"}
	recorded.Durations.SearchTrialS = 17
	dir := recordTwoTrials(t, recorded)
	path := filepath.Join(dir, "events.jsonl")
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	data = bytes.Replace(data, fmt.Appendf(nil, `"schema":%d`, journal.Schema), fmt.Appendf(nil, `"schema":%d`, journal.Schema-1), 1)
	if err := os.WriteFile(path, data, 0o644); err != nil {
		t.Fatal(err)
	}
	if got := run([]string{"--state-dir", dir, "--max-boots", "1"}, io.Discard); got != 0 && got != 3 {
		t.Fatalf("exit %d", got)
	}
	if archived, err := filepath.Glob(filepath.Join(dir, "archive", "*.jsonl")); err != nil || len(archived) != 1 {
		t.Fatalf("archived journals %v: %v", archived, err)
	}
	events, _, err := journal.Read(dir)
	if err != nil {
		t.Fatal(err)
	}
	var loaded *journal.ConfigLoaded
	for _, e := range events {
		if p, ok := e.Data.(*journal.ConfigLoaded); ok {
			loaded = p
		}
	}
	if loaded == nil {
		t.Fatal("the new session recorded no config.loaded")
	}
	want := config.Default()
	want.Backends = recorded.Backends
	if diff := cmp.Diff(want, session.ConfigFromSnapshot(loaded.Config)); diff != "" {
		t.Fatalf("configuration of the session that replaced the older schema (-want +got):\n%s", diff)
	}
}

func recordTwoTrials(t *testing.T, recorded config.Config) string {
	t.Helper()
	dir := t.TempDir()
	m, err := sim.New(sim.Config{Seed: 1})
	if err != nil {
		t.Fatal(err)
	}
	ends := 0
	until := func(e journal.Event) bool {
		if e.Kind == journal.KindTrialEnd {
			ends++
		}
		return ends == 2
	}
	if _, err := simrun.Simulate(context.Background(), simrun.Input{Config: recorded, ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Cycles: 1, InMemoryJournal: true, Until: until}); err != nil {
		t.Fatal(err)
	}
	return dir
}
