package main

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"
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
		{"zero laps", []string{"--laps", "0"}, 2, "sim: --laps must be a positive integer"},
		{"negative laps", []string{"--laps", "-1"}, 2, "sim: --laps must be a positive integer"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var out bytes.Buffer
			if got := run(tc.args, &out); got != tc.code || !strings.Contains(out.String(), tc.diagnostic) {
				t.Fatalf("exit %d, output %q; want %d, %q", got, out.String(), tc.code, tc.diagnostic)
			}
		})
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
