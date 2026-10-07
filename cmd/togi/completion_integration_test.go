//go:build integration

package main

import (
	"bytes"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

// TestNushellCompletionLoads loads the nushell script with the nu on PATH, the
// one from togi's pinned nixpkgs in the dev shell and the package check, and
// runs the --kind completer, since nu warns about deprecated completer
// signatures when it first calls them. An autoloaded script would print any
// error or warning on every nushell start or completion.
func TestNushellCompletionLoads(t *testing.T) {
	t.Parallel()
	nu, err := exec.LookPath("nu")
	if err != nil {
		t.Fatalf("nu not on PATH; run in the dev shell: %v", err)
	}
	dir := t.TempDir()
	script := filepath.Join(dir, "togi.nu")
	if err := os.WriteFile(script, []byte(completionScript(t, "nushell")), 0o600); err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(nu, "--no-config-file", "--no-history", "-c",
		"source '"+script+"'; 'togi events --kind se' | commandline complete | str join ' '")
	cmd.Dir = dir
	cmd.Env = append(os.Environ(), "HOME="+dir, "XDG_CONFIG_HOME="+dir, "XDG_DATA_HOME="+dir, "NO_COLOR=1")
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if err := cmd.Run(); err != nil {
		t.Fatalf("nu: %v\nstdout %s\nstderr %s", err, stdout.String(), stderr.String())
	}
	output := stdout.String() + stderr.String()
	if stderr.Len() != 0 || strings.Contains(output, "deprecated") {
		t.Fatalf("nu printed a warning or error loading the script:\nstdout %s\nstderr %s", stdout.String(), stderr.String())
	}
	if !strings.Contains(" "+strings.TrimSpace(stdout.String())+" ", " session ") {
		t.Fatalf("togi events --kind se completes %q, want session among them", stdout.String())
	}
}
