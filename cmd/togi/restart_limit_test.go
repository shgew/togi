package main

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/tuningboot"
)

type restartLimitBootloader struct {
	clearingBootloader
	getErr   error
	clearErr error
}

func (b *restartLimitBootloader) Get(name string) (string, error) {
	value, _ := b.clearingBootloader.Get(name)
	return value, b.getErr
}

func (b *restartLimitBootloader) ClearSavedEntry() (string, string, error) {
	b.calls++
	b.environmentCalls = append(b.environmentCalls, "clear saved_entry")
	if b.clearErr != nil {
		return "togi", "togi", b.clearErr
	}
	return "togi", "", nil
}

func TestRestartLimitCommandBootActions(t *testing.T) {
	t.Parallel()
	for _, initial := range []string{"", "0", "1", "2", "3"} {
		t.Run("count "+initial, func(t *testing.T) {
			bootloader := &restartLimitBootloader{}
			bootloader.values = map[string]string{tuningboot.CountVariable: initial}
			marker := initial == "2" || initial == "3" // also clear any stale retry marker on exhaustion
			retry := initial != "2" && initial != "3"
			operations := restartLimitOperations{
				bootloader: func(env string) tuningboot.Bootloader {
					if env != "fake grubenv" {
						t.Fatalf("environment path %q", env)
					}
					return bootloader
				},
				create: func() error {
					bootloader.environmentCalls = append(bootloader.environmentCalls, "create marker")
					marker = true
					return nil
				},
				remove: func() error {
					bootloader.environmentCalls = append(bootloader.environmentCalls, "remove marker")
					marker = false
					return nil
				},
				command: func(ctx context.Context, name string, args ...string) ([]byte, error) {
					bootloader.environmentCalls = append(bootloader.environmentCalls, "reboot")
					if marker != retry {
						t.Fatalf("marker at reboot = %v, want %v", marker, retry)
					}
					if diff := cmp.Diff([]string{"systemctl", "reboot"}, append([]string{name}, args...)); diff != "" {
						t.Fatalf("ordinary reboot command (-want +got): %s", diff)
					}
					if _, ok := ctx.Deadline(); !ok {
						t.Fatal("reboot has no deadline")
					}
					return nil, nil
				},
			}
			var out, diagnostics bytes.Buffer
			g := testGlobals(t)
			if code := runRestartLimitWith(&g, []string{"--tuning-boot", "fake grubenv"}, &out, &diagnostics, operations); code != exitOK || out.Len() != 0 {
				t.Fatalf("exit %d stdout=%q stderr=%q", code, out.String(), diagnostics.String())
			}
			wantOperations := []string{"get " + tuningboot.CountVariable, "set", "create marker", "reboot"}
			if !retry {
				wantOperations = []string{"get " + tuningboot.CountVariable, "set", "clear saved_entry", "remove marker", "reboot"}
			}
			if diff := cmp.Diff(wantOperations, bootloader.environmentCalls); diff != "" {
				t.Fatalf("boot operation order (-want +got): %s", diff)
			}
			count := 1
			if initial == "1" {
				count = 2
			} else if !retry {
				count = 3
			}
			want := fmt.Sprintf("togi restart-limit: attempt %d of 3; rebooting back into the tuning boot\n", count)
			if !retry {
				want = fmt.Sprintf("togi restart-limit: attempt %d of 3; rebooting into the normal system\n", count)
			}
			if diff := cmp.Diff(want, diagnostics.String()); diff != "" {
				t.Fatalf("recovery report (-want +got): %s", diff)
			}
			if diff := cmp.Diff(map[string]string{}, directoryFiles(t, g.stateDir)); diff != "" {
				t.Fatalf("restart command touched journal state: %s", diff)
			}
		})
	}
}

func TestRestartLimitHelpAndUsageHaveNoSideEffects(t *testing.T) {
	t.Parallel()
	for _, tt := range []struct {
		name string
		args []string
		code int
	}{
		{"help", []string{"--help"}, exitOK},
		{"help with environment", []string{"--tuning-boot", "must not open", "--help"}, exitOK},
		{"missing environment", nil, exitUsage},
		{"empty environment", []string{"--tuning-boot="}, exitUsage},
		{"missing flag value", []string{"--tuning-boot"}, exitUsage},
		{"unexpected argument", []string{"--tuning-boot", "unused", "extra"}, exitUsage},
		{"unknown flag", []string{"--force"}, exitUsage},
	} {
		t.Run(tt.name, func(t *testing.T) {
			var out, diagnostics bytes.Buffer
			g := testGlobals(t)
			// Nil operations prove parsing returns before any environment,
			// marker or reboot access, even when help names an environment.
			code := runRestartLimitWith(&g, tt.args, &out, &diagnostics, restartLimitOperations{})
			if code != tt.code {
				t.Fatalf("exit %d, want %d; stderr %s", code, tt.code, diagnostics.String())
			}
			if tt.code == exitOK {
				if diagnostics.Len() != 0 {
					t.Fatalf("help diagnostics: %s", diagnostics.String())
				}
				golden(t, "help-restart-limit", out.String())
			} else if out.Len() != 0 || !strings.Contains(diagnostics.String(), "Usage: togi restart-limit") {
				t.Fatalf("usage streams stdout=%q stderr=%q", out.String(), diagnostics.String())
			}
		})
	}
}

func TestRestartLimitFailuresStopOrRemoveRetryMarker(t *testing.T) {
	t.Parallel()
	failure := errors.New("injected failure")
	for _, tt := range []struct {
		name, count string
		want        []string
		marker      bool
	}{
		{"read", "0", []string{"get " + tuningboot.CountVariable}, false},
		{"invalid count", "invalid", []string{"get " + tuningboot.CountVariable}, false},
		{"write", "0", []string{"get " + tuningboot.CountVariable, "set"}, false},
		{"clear", "2", []string{"get " + tuningboot.CountVariable, "set", "clear saved_entry"}, false},
		{"create", "0", []string{"get " + tuningboot.CountVariable, "set", "create marker"}, false},
		{"exhausted marker cleanup", "2", []string{"get " + tuningboot.CountVariable, "set", "clear saved_entry", "remove marker"}, false},
		{"reboot", "0", []string{"get " + tuningboot.CountVariable, "set", "create marker", "reboot", "remove marker"}, false},
		{"reboot and marker cleanup", "0", []string{"get " + tuningboot.CountVariable, "set", "create marker", "reboot", "remove marker"}, true},
		{"exhausted reboot", "2", []string{"get " + tuningboot.CountVariable, "set", "clear saved_entry", "remove marker", "reboot"}, false},
	} {
		t.Run(tt.name, func(t *testing.T) {
			bootloader := &restartLimitBootloader{}
			bootloader.values = map[string]string{tuningboot.CountVariable: tt.count}
			switch tt.name {
			case "read":
				bootloader.getErr = failure
			case "write":
				bootloader.setErr = failure
			case "clear":
				bootloader.clearErr = failure
			}
			marker := false
			operations := restartLimitOperations{
				bootloader: func(string) tuningboot.Bootloader { return bootloader },
				create: func() error {
					bootloader.environmentCalls = append(bootloader.environmentCalls, "create marker")
					if tt.name == "create" {
						return failure
					}
					marker = true
					return nil
				},
				remove: func() error {
					bootloader.environmentCalls = append(bootloader.environmentCalls, "remove marker")
					if tt.name == "reboot and marker cleanup" || tt.name == "exhausted marker cleanup" {
						return failure
					}
					marker = false
					return nil
				},
				command: func(context.Context, string, ...string) ([]byte, error) {
					bootloader.environmentCalls = append(bootloader.environmentCalls, "reboot")
					return []byte(" denied\n"), failure
				},
			}
			var out, diagnostics bytes.Buffer
			g := testGlobals(t)
			code := runRestartLimitWith(&g, []string{"--tuning-boot", "fake grubenv"}, &out, &diagnostics, operations)
			if code != exitError || out.Len() != 0 || diagnostics.Len() == 0 {
				t.Fatalf("exit %d stdout=%q stderr=%q", code, out.String(), diagnostics.String())
			}
			if diff := cmp.Diff(tt.want, bootloader.environmentCalls); diff != "" {
				t.Fatalf("failure operation order (-want +got): %s", diff)
			}
			if marker != tt.marker {
				t.Fatalf("marker after failure=%v want %v", marker, tt.marker)
			}
			if strings.Contains(tt.name, "reboot") && !strings.Contains(diagnostics.String(), "systemctl reboot: injected failure: denied") {
				t.Fatalf("reboot failure output hidden: %s", diagnostics.String())
			}
			if tt.name == "reboot and marker cleanup" && !strings.Contains(diagnostics.String(), "remove retry marker after failed reboot: injected failure") {
				t.Fatalf("cleanup failure hidden: %s", diagnostics.String())
			}
		})
	}
}

func TestRestartLimitDispatchRejectsConfig(t *testing.T) {
	t.Parallel()
	for _, args := range [][]string{
		{"restart-limit", "--config", "unused.json", "--help"},
		{"--config", "unused.json", "restart-limit", "--help"},
	} {
		var out, diagnostics bytes.Buffer
		if code := cli(args, &out, &diagnostics); code != exitUsage || out.Len() != 0 || !strings.Contains(diagnostics.String(), "--config applies only to doctor, run and reset") {
			t.Fatalf("exit %d stdout=%q stderr=%q", code, out.String(), diagnostics.String())
		}
	}
}
