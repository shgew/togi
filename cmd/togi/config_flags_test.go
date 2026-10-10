package main

import (
	"bytes"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
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
			wantConfig := acceptsConfig(c.name)
			if got := strings.Contains(stdout.String(), "--config <path>"); got != wantConfig {
				t.Fatalf("config flag in help: %v, want %v; stdout %s", got, wantConfig, stdout.String())
			}
			if wantConfig && !strings.Contains(stdout.String(), "(default /etc/togi/config.json)") {
				t.Fatalf("help omits default JSON config path: %s", stdout.String())
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
				args := []string{name, "--config", "unused.json", "--help"}
				if placement == "before" {
					args = []string{"--config", "unused.json", name, "--help"}
				}
				var stdout, stderr bytes.Buffer
				if code := cli(args, &stdout, &stderr); code != exitUsage {
					t.Fatalf("exit %d, want %d; stderr %s", code, exitUsage, stderr.String())
				}
				if stdout.Len() != 0 || !strings.Contains(stderr.String(), "--config applies only to doctor, run and reset") {
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
				args := []string{name, "--config", "unused.json", "--help"}
				if placement == "before" {
					args = []string{"--config", "unused.json", name, "--help"}
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

func TestLeadingConfigRejectionPrecedesCommandArguments(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name string
		args []string
	}{
		{name: "events"},
		{name: "events", args: []string{"--help"}},
		{name: "events", args: []string{"--kind", "unknown"}},
		{name: "events", args: []string{"unexpected"}},
		{name: "status", args: []string{"--unknown"}},
		{name: "watch", args: []string{"--width", "0"}},
	} {
		t.Run(tc.name+"/"+strings.Join(tc.args, " "), func(t *testing.T) {
			t.Parallel()
			args := append([]string{"--config", "unused.json", tc.name}, tc.args...)
			var stdout, stderr bytes.Buffer
			if code := cli(args, &stdout, &stderr); code != exitUsage {
				t.Fatalf("exit %d, want %d; stderr %s", code, exitUsage, stderr.String())
			}
			if diff := cmp.Diff("", stdout.String()); diff != "" {
				t.Fatalf("stdout (-want +got): %s", diff)
			}
			diagnostic, help, _ := strings.Cut(stderr.String(), "\n")
			want := "togi " + tc.name + ": --config applies only to doctor, run and reset"
			if diff := cmp.Diff(want, diagnostic); diff != "" {
				t.Fatalf("config diagnostic (-want +got): %s", diff)
			}
			golden(t, "help-"+tc.name, help)
		})
	}
}
