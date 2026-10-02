package hardware

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func TestWatchdogReadiness(t *testing.T) {
	t.Parallel()
	for _, tt := range []struct {
		name       string
		states     []string
		identities []string
		ok         bool
		detail     string
	}{
		{name: "absent", detail: "no hardware watchdog state available"},
		{name: "inactive", states: []string{"inactive\n"}, identities: []string{"SP5100 TCO timer\n"}, detail: `watchdog0 state "inactive", want active`},
		{name: "armed", states: []string{"active\n"}, identities: []string{"SP5100 TCO timer\n"}, ok: true, detail: "watchdog0 (SP5100 TCO timer) active"},
		{name: "software only", states: []string{"active\n"}, identities: []string{"Software Watchdog\n"}, detail: `watchdog0 is not a hardware watchdog ("Software Watchdog")`},
		{name: "unknown identity", states: []string{"active\n"}, identities: []string{""}, detail: `watchdog0 is not a hardware watchdog ("")`},
		{name: "missing identity", states: []string{"active\n"}, detail: "read watchdog0 identity:"},
		{name: "malformed state", states: []string{"enabled\n"}, identities: []string{"SP5100 TCO timer\n"}, detail: `watchdog0 state "enabled", want active`},
		{name: "later hardware device", states: []string{"active\n", "active\n"}, identities: []string{"Software Watchdog\n", "i6300ESB timer\n"}, ok: true, detail: "watchdog1 (i6300ESB timer) active"},
	} {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			root := t.TempDir()
			for i, state := range tt.states {
				name := "watchdog" + string(rune('0'+i))
				dir := filepath.Join(root, "sys/class/watchdog", name)
				if err := os.MkdirAll(dir, 0755); err != nil {
					t.Fatal(err)
				}
				if err := os.WriteFile(filepath.Join(dir, "state"), []byte(state), 0644); err != nil {
					t.Fatal(err)
				}
				if i < len(tt.identities) {
					if err := os.WriteFile(filepath.Join(dir, "identity"), []byte(tt.identities[i]), 0644); err != nil {
						t.Fatal(err)
					}
				}
			}
			got := watchdog(root)
			if diff := cmp.Diff(machine.Check{Name: "watchdog", OK: tt.ok}, machine.Check{Name: got.Name, OK: got.OK}); diff != "" {
				t.Fatalf("readiness (-want +got): %s", diff)
			}
			if !strings.Contains(got.Detail, tt.detail) {
				t.Fatalf("watchdog diagnostic %q missing %q", got.Detail, tt.detail)
			}
		})
	}
}

func TestWatchdogUnreadableState(t *testing.T) {
	for _, laterHardware := range []bool{false, true} {
		name := "refusal"
		if laterHardware {
			name = "later hardware"
		}
		t.Run(name, func(t *testing.T) {
			root := t.TempDir()
			dir := filepath.Join(root, "sys/class/watchdog/watchdog0")
			if err := os.MkdirAll(filepath.Join(dir, "state"), 0755); err != nil {
				t.Fatal(err)
			}
			if laterHardware {
				dir = filepath.Join(root, "sys/class/watchdog/watchdog1")
				if err := os.MkdirAll(dir, 0755); err != nil {
					t.Fatal(err)
				}
				for file, text := range map[string]string{"state": "active\n", "identity": "Test hardware timer\n"} {
					if err := os.WriteFile(filepath.Join(dir, file), []byte(text), 0644); err != nil {
						t.Fatal(err)
					}
				}
			}
			got := watchdog(root)
			if laterHardware {
				want := machine.Check{Name: "watchdog", OK: true, Detail: "watchdog1 (Test hardware timer) active"}
				if diff := cmp.Diff(want, got); diff != "" {
					t.Fatalf("later watchdog readiness (-want +got):\n%s", diff)
				}
			} else if got.Name != "watchdog" || got.OK || !strings.Contains(got.Detail, "read watchdog0 state:") {
				t.Fatalf("unreadable watchdog refusal hidden: %+v", got)
			}
		})
	}
}
