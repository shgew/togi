package hardware

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestWatchdogReadiness(t *testing.T) {
	t.Parallel()
	for _, tt := range []struct {
		name       string
		states     []string
		identities []string
		ok         bool
	}{
		{name: "absent"},
		{name: "inactive", states: []string{"inactive\n"}, identities: []string{"SP5100 TCO timer\n"}},
		{name: "armed", states: []string{"active\n"}, identities: []string{"SP5100 TCO timer\n"}, ok: true},
		{name: "software only", states: []string{"active\n"}, identities: []string{"Software Watchdog\n"}},
		{name: "unknown identity", states: []string{"active\n"}, identities: []string{""}},
		{name: "missing identity", states: []string{"active\n"}},
		{name: "malformed state", states: []string{"enabled\n"}, identities: []string{"SP5100 TCO timer\n"}},
		{name: "later hardware device", states: []string{"active\n", "active\n"}, identities: []string{"Software Watchdog\n", "i6300ESB timer\n"}, ok: true},
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
			if diff := cmp.Diff(tt.ok, watchdog(root).OK); diff != "" {
				t.Fatalf("readiness (-want +got): %s", diff)
			}
		})
	}
}
