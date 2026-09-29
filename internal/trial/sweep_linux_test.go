//go:build linux

package trial

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"testing/synctest"

	"github.com/google/go-cmp/cmp"
)

func procFixture(t *testing.T, root string, pid int, cgroup, state string, group, start int) {
	t.Helper()
	dir := filepath.Join(root, strconv.Itoa(pid))
	if err := os.MkdirAll(dir, 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "cgroup"), []byte(cgroup), 0644); err != nil {
		t.Fatal(err)
	}
	fields := make([]string, 40)
	for i := range fields {
		fields[i] = "0"
	}
	fields[0], fields[2], fields[19] = state, strconv.Itoa(group), strconv.Itoa(start)
	if err := os.WriteFile(filepath.Join(dir, "stat"), []byte(fmt.Sprintf("%d (name with spaces) %s", pid, strings.Join(fields, " "))), 0644); err != nil {
		t.Fatal(err)
	}
}

func TestScopeProcessDiscoveryAndIdentity(t *testing.T) {
	root := t.TempDir()
	procFixture(t, root, 42, "0::/system.slice/togi-trial-1.scope/descendant\n", "S", 40, 100)
	procFixture(t, root, 43, "0::/system.slice/other-togi-trial-1.scope\n", "S", 40, 100)
	procFixture(t, root, 44, "0::/system.slice/togi-preflight-1.scope\n", "S", 40, 100)
	procFixture(t, root, 45, "0::/system.slice/togi-trial-1.scope\n", "Z", 40, 100)
	h := osHost{procDir: root}
	processes, err := h.ScopeProcesses(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	want := []scopeProcess{{Scope: "togi-trial-1.scope", PID: 42, Group: 40, Start: 100}}
	if diff := cmp.Diff(want, processes); diff != "" {
		t.Fatalf("scoped descendants (-want +got):\n%s", diff)
	}
	alive, err := h.ProcessAlive(want[0])
	if err != nil || !alive {
		t.Fatalf("live identity = %t, %v", alive, err)
	}
	procFixture(t, root, 42, "0::/elsewhere\n", "S", 40, 101)
	alive, err = h.ProcessAlive(want[0])
	if err != nil || alive {
		t.Fatalf("reused PID counted as leftover = %t, %v", alive, err)
	}
}

func TestSystemdScopeListFiltersNames(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		ctx, cancel := context.WithTimeout(context.Background(), teardownLimit)
		defer cancel()
		h := osHost{command: func(ctx context.Context, name string, args ...string) ([]byte, error) {
			if _, ok := ctx.Deadline(); !ok {
				t.Fatal("unbounded listing")
			}
			return []byte("togi-trial-1.scope loaded active running stale\ntogi-preflight-1.scope loaded active running probe\nother-togi-trial-1.scope loaded active running unrelated\ntogi-trial-*.scope loaded active running glob\n"), nil
		}}
		units, err := h.ListScopes(ctx)
		if err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff([]string{"togi-trial-1.scope"}, units); diff != "" {
			t.Fatalf("units (-want +got):\n%s", diff)
		}
	})
}
