package main

import (
	"bytes"
	"strings"
	"testing"
)

func TestCommandHelpConfigScope(t *testing.T) {
	t.Parallel()
	for _, c := range commands {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			var stdout, stderr bytes.Buffer
			if code := cli([]string{c.name, "--help"}, &stdout, &stderr); code != exitOK {
				t.Fatalf("exit %d; stderr %s", code, stderr.String())
			}
			wantConfig := c.name == "run" || c.name == "reset"
			if got := strings.Contains(stdout.String(), "--config <path>"); got != wantConfig {
				t.Fatalf("config flag in help: %v, want %v; stdout %s", got, wantConfig, stdout.String())
			}
			if !strings.Contains(stdout.String(), "--state-dir <path>") {
				t.Fatalf("help omits shared state-dir flag: %s", stdout.String())
			}
			golden(t, "help-"+c.name, stdout.String())
		})
	}
}

func TestReadOnlyCommandsRejectConfig(t *testing.T) {
	t.Parallel()
	for _, name := range []string{"status", "events", "watch"} {
		for _, placement := range []string{"before", "after"} {
			t.Run(name+"/"+placement, func(t *testing.T) {
				t.Parallel()
				args := []string{name, "--config", "unused.toml", "--help"}
				if placement == "before" {
					args = []string{"--config", "unused.toml", name, "--help"}
				}
				var stdout, stderr bytes.Buffer
				if code := cli(args, &stdout, &stderr); code != exitUsage {
					t.Fatalf("exit %d, want %d; stderr %s", code, exitUsage, stderr.String())
				}
				if stdout.Len() != 0 || !strings.Contains(stderr.String(), "flag provided but not defined: -config") {
					t.Fatalf("stdout %q, stderr %q; want unknown config flag on stderr only", stdout.String(), stderr.String())
				}
				if strings.Contains(stderr.String(), "--config <path>") {
					t.Fatalf("error help advertises config: %s", stderr.String())
				}
			})
		}
	}
}

func TestWriteCommandsAcceptConfigPlacements(t *testing.T) {
	t.Parallel()
	for _, name := range []string{"run", "reset"} {
		for _, placement := range []string{"before", "after"} {
			t.Run(name+"/"+placement, func(t *testing.T) {
				t.Parallel()
				args := []string{name, "--config", "unused.toml", "--help"}
				if placement == "before" {
					args = []string{"--config", "unused.toml", name, "--help"}
				}
				var stdout, stderr bytes.Buffer
				if code := cli(args, &stdout, &stderr); code != exitOK || stderr.Len() != 0 {
					t.Fatalf("exit %d; stderr %s", code, stderr.String())
				}
				if !strings.HasPrefix(stdout.String(), "Usage: togi "+name) {
					t.Fatalf("stdout %q, want command help", stdout.String())
				}
			})
		}
	}
}
