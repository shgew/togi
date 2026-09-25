package main

import (
	"bytes"
	"os"
	"regexp"
	"strings"
	"testing"
)

func TestVersion(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name string
		args func(string) []string
		code int
	}{
		{name: "top-level", args: func(string) []string { return []string{"--version"} }, code: exitOK},
		{name: "status", args: func(dir string) []string { return []string{"status", "--state-dir", dir, "--version"} }, code: exitUsage},
		{name: "run", args: func(dir string) []string { return []string{"run", "--state-dir", dir, "--version"} }, code: exitUsage},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			dir := t.TempDir()
			var stdout, stderr bytes.Buffer
			if code := cli(tt.args(dir), &stdout, &stderr); code != tt.code {
				t.Fatalf("exit %d, want %d; stderr %s", code, tt.code, stderr.String())
			}
			if tt.code == exitOK {
				if got := stdout.String(); !regexp.MustCompile(`^shycler \d+\.\d+\.\d+\+[0-9a-z-]+\n$`).MatchString(got) {
					t.Fatalf("stdout %q, want shycler <version>+<rev>", got)
				}
				if stderr.Len() != 0 {
					t.Fatalf("stderr %q", stderr.String())
				}
			} else {
				if stdout.Len() != 0 {
					t.Fatalf("stdout %q, want none", stdout.String())
				}
				if got := stderr.String(); !strings.Contains(got, "flag provided but not defined: -version") {
					t.Fatalf("stderr %q, want unknown-flag error", got)
				}
				if strings.Contains(stderr.String(), "--version") {
					t.Fatalf("subcommand usage lists --version: %s", stderr.String())
				}
			}
			entries, err := os.ReadDir(dir)
			if err != nil {
				t.Fatalf("read state directory: %v", err)
			}
			if len(entries) != 0 {
				t.Fatalf("state directory changed: %v", entries)
			}
		})
	}
}

func TestVersionInTopLevelUsageOnly(t *testing.T) {
	t.Parallel()
	for _, tt := range []struct {
		name string
		args []string
		want bool
	}{
		{name: "top-level", args: []string{"--help"}, want: true},
		{name: "status", args: []string{"status", "--help"}},
		{name: "run", args: []string{"run", "--help"}},
	} {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			var stdout, stderr bytes.Buffer
			if code := cli(tt.args, &stdout, &stderr); code != exitOK {
				t.Fatalf("exit %d; stderr %s", code, stderr.String())
			}
			if stderr.Len() != 0 {
				t.Fatalf("stderr %q", stderr.String())
			}
			if got := strings.Contains(stdout.String(), "  --version"); got != tt.want {
				t.Fatalf("version flag in usage: %v, want %v; stdout %s", got, tt.want, stdout.String())
			}
			if !strings.Contains(stdout.String(), "--config <path>") || !strings.Contains(stdout.String(), "--state-dir <path>") {
				t.Fatalf("usage omits shared flags: %s", stdout.String())
			}
		})
	}
}
