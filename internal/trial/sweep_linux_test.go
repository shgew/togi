//go:build linux

package trial

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
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
	want := []scopeProcess{{Scope: "togi-trial-1.scope", PID: 42, Start: 100}}
	if diff := cmp.Diff(want, processes); diff != "" {
		t.Fatalf("scoped descendants (-want +got):\n%s", diff)
	}
	alive, err := h.ProcessAlive(want[0])
	if err != nil || !alive {
		t.Fatalf("live identity = %t, %v", alive, err)
	}
	procFixture(t, root, 42, "0::/elsewhere\n", "S", 40, 100)
	alive, err = h.ProcessAlive(want[0])
	if err != nil || !alive {
		t.Fatalf("escaped captured process was not retained = %t, %v", alive, err)
	}
	procFixture(t, root, 42, "0::/elsewhere\n", "S", 40, 101)
	alive, err = h.ProcessAlive(want[0])
	if err != nil || alive {
		t.Fatalf("reused PID counted as leftover = %t, %v", alive, err)
	}
}

func TestProcessDisappeared(t *testing.T) {
	for _, tc := range []struct {
		name string
		err  error
		gone bool
	}{
		{"exited-during-read", &os.PathError{Op: "read", Path: "/proc/42/cgroup", Err: syscall.ESRCH}, true},
		{"exited-before-open", &os.PathError{Op: "open", Path: "/proc/42/cgroup", Err: syscall.ENOENT}, true},
		{"denied", syscall.EACCES, false},
		{"io-failure", syscall.EIO, false},
		{"present", nil, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := processDisappeared(tc.err); got != tc.gone {
				t.Fatalf("disappeared(%v) = %v, want %v", tc.err, got, tc.gone)
			}
		})
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

func TestOwnedProcessGroupRejectsUnverifiedIdentity(t *testing.T) {
	for _, tt := range []struct {
		name, state     string
		start, group    int
		reaped, missing bool
		want            error
	}{
		{name: "PID reused", state: "S", start: 101, group: 42, want: syscall.ESRCH},
		{name: "zombie", state: "Z", start: 100, group: 42, want: syscall.ESRCH},
		{name: "dead", state: "X", start: 100, group: 42, want: syscall.ESRCH},
		{name: "reaped", state: "S", start: 100, group: 42, reaped: true, want: syscall.ESRCH},
		{name: "disappeared", missing: true, want: syscall.ESRCH},
		{name: "group changed", state: "S", start: 100, group: 40},
	} {
		t.Run(tt.name, func(t *testing.T) {
			root := t.TempDir()
			if !tt.missing {
				procFixture(t, root, 42, "0::/unrelated.scope\n", tt.state, tt.group, tt.start)
			}
			h := osHost{procDir: root}
			p := &execProcess{cmd: &exec.Cmd{Process: &os.Process{Pid: 42}}, start: 100, reaped: tt.reaped}
			err := h.SignalGroup(p, syscall.SIGKILL)
			if err == nil || tt.want != nil && !errors.Is(err, tt.want) {
				t.Fatalf("unverified process-group signal = %v, want %v", err, tt.want)
			}
		})
	}
}
