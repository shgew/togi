//go:build hardware && linux

package trial

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/shgew/togi/internal/backend"
	"github.com/shgew/togi/internal/hostlock"
	"github.com/shgew/togi/internal/machine"
)

func hardwareRoot(t *testing.T) Identity {
	t.Helper()
	if os.Geteuid() != 0 {
		t.Skip("requires a privileged system-scope launcher")
	}
	lock, err := hostlock.Acquire(hostlock.Path)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := lock.Close(); err != nil {
			t.Error(err)
		}
	})
	return hardwareUser(t)
}

func hardwareOptions(t *testing.T, user Identity, mode string) Options {
	t.Helper()
	cpus := testCPUs(t)
	return Options{
		Dir: hardwareDir(t), User: user,
		Backends:       map[machine.Backend]backend.Backend{machine.Mprime: helperBackend{mode: mode, executable: stageHelper(t)}},
		Cores:          []machine.CoreInfo{{Core: 0, CPUs: []int{cpus[0]}}, {Core: 1, CPUs: []int{cpus[1]}}},
		SampleInterval: 50 * time.Millisecond,
	}
}

func hardwareIdentity(t *testing.T, pid int, scope string) scopeProcess {
	t.Helper()
	fields, err := procStat(fmt.Sprintf("/proc/%d/stat", pid))
	if err != nil {
		t.Fatal(err)
	}
	start, err := fieldInt(fields, 22)
	if err != nil {
		t.Fatal(err)
	}
	if !(osHost{}).InScope(pid, scope) {
		t.Fatalf("process %d is not in real scope %s", pid, scope)
	}
	return scopeProcess{Scope: scope + ".scope", PID: pid, Start: start}
}

func hardwareDescendant(t *testing.T, dir, scope string) scopeProcess {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for {
		data, err := os.ReadFile(filepath.Join(dir, "descendant.pid"))
		if err == nil {
			pid, err := strconv.Atoi(string(data))
			if err != nil {
				t.Fatal(err)
			}
			return hardwareIdentity(t, pid, scope)
		}
		if !os.IsNotExist(err) || !time.Now().Before(deadline) {
			t.Fatalf("wait for scoped descendant: %v", err)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func hardwareProcessesGone(t *testing.T, processes ...scopeProcess) {
	t.Helper()
	for _, p := range processes {
		alive, err := (osHost{}).ProcessAlive(p)
		if err != nil || alive {
			t.Fatalf("scoped process survived cleanup: %+v alive=%t err=%v", p, alive, err)
		}
	}
}

func hardwareScopesGone(t *testing.T, scopes ...string) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	for {
		units, err := newOSHost().ListScopes(ctx)
		if err != nil {
			t.Fatal(err)
		}
		remaining := slices.ContainsFunc(scopes, func(scope string) bool { return slices.Contains(units, scope+".scope") })
		if !remaining {
			return
		}
		select {
		case <-ctx.Done():
			t.Fatalf("scopes remain after cleanup: %v", units)
		case <-time.After(50 * time.Millisecond):
		}
	}
}

func hardwareScopeCleanup(t *testing.T, scope string) {
	t.Helper()
	t.Cleanup(func() {
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()
		h := newOSHost()
		_, _ = h.KillScope(ctx, scope)
		_, _ = h.StopScope(ctx, scope)
	})
}

type hardwareRollbackHost struct {
	processHost
	t         *testing.T
	starts    int
	kills     []string
	first     process
	orphan    scopeProcess
	failureAt time.Time
}

func (h *hardwareRollbackHost) Start(ctx context.Context, argv []string, dir string) (process, error) {
	h.starts++
	if h.starts == 2 {
		h.failureAt = time.Now()
		return nil, errors.New("injected second-instance start failure")
	}
	p, err := h.processHost.Start(ctx, argv, dir)
	if err != nil {
		return nil, err
	}
	h.first = p
	h.orphan = hardwareDescendant(h.t, dir, "togi-trial-hw-rollback-c00")
	return p, nil
}

func (h *hardwareRollbackHost) KillScope(ctx context.Context, scope string) ([]byte, error) {
	h.kills = append(h.kills, scope)
	return h.processHost.KillScope(ctx, scope)
}

func TestHardwareScopePartialStartRollback(t *testing.T) {
	o := hardwareOptions(t, hardwareRoot(t), "pipe-descendant")
	h := &hardwareRollbackHost{processHost: newOSHost(), t: t}
	r := New(o)
	r.host = h
	scopes := []string{"togi-trial-hw-rollback-c00", "togi-trial-hw-rollback-c01"}
	for _, scope := range scopes {
		hardwareScopeCleanup(t, scope)
	}
	spec := testSpec("hw-rollback", machine.R7, time.Second)
	spec.Cores = []int{0, 1}
	spec.CPUs = []int{o.Cores[0].CPUs[0], o.Cores[1].CPUs[0]}
	_, err := r.Start(context.Background(), spec)
	elapsed := time.Since(h.failureAt)
	if err == nil || !strings.Contains(err.Error(), "injected second-instance start failure") || errors.Is(err, machine.ErrContainment) {
		t.Fatalf("real scoped rollback did not confirm cleanup: %v", err)
	}
	if h.failureAt.IsZero() || elapsed > teardownLimit {
		t.Fatalf("rollback exceeded shared deadline: %s", elapsed)
	}
	if !slices.Equal(h.kills, scopes) {
		t.Fatalf("attempted scopes were not all cleaned: %v", h.kills)
	}
	hardwareProcessesGone(t, h.orphan)
	p := h.first.(*execProcess)
	hardwareProcessesGone(t, scopeProcess{PID: p.PID(), Start: p.start})
	for _, reader := range []*os.File{p.stdout, p.stderr} {
		if _, err := reader.Stat(); !errors.Is(err, os.ErrClosed) {
			t.Fatalf("output reader remains open after rollback: %v", err)
		}
	}
	hardwareScopesGone(t, scopes...)
	t.Logf("real first scope and detached pipe holder removed; both attempted scopes targeted; readers closed in %s", elapsed)
}

func hardwareOrphanScope(t *testing.T, o Options, scope string) (process, scopeProcess) {
	t.Helper()
	dir := filepath.Join(o.Dir, scope)
	if err := os.Mkdir(dir, 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.Chown(dir, int(o.User.UID), int(o.User.GID)); err != nil {
		t.Fatal(err)
	}
	launch, err := o.Backends[machine.Mprime].Prepare(machine.Workload{}, dir, o.Cores[0].CPUs)
	if err != nil {
		t.Fatal(err)
	}
	h := newOSHost()
	hardwareScopeCleanup(t, scope)
	owner, err := h.Start(context.Background(), scopeArgv(scope, o.Cores[0].CPUs, o.User, dir, launch.Argv...), dir)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		owner.Stdout().Close()
		owner.Stderr().Close()
	})
	orphan := hardwareDescendant(t, dir, scope)
	if err := h.SignalGroup(owner, syscall.SIGKILL); err != nil {
		t.Fatal(err)
	}
	_ = owner.Wait()
	if alive, err := h.ProcessAlive(orphan); err != nil || !alive {
		t.Fatalf("detached process did not survive helper owner: alive=%t err=%v", alive, err)
	}
	return owner, orphan
}

func TestHardwareScopeSweepOrphan(t *testing.T) {
	o := hardwareOptions(t, hardwareRoot(t), "scope-owner")
	_, orphan := hardwareOrphanScope(t, o, "togi-trial-hw-orphan")
	_, unrelated := hardwareOrphanScope(t, o, "togi-unrelated-hw-sweep")
	_, preflight := hardwareOrphanScope(t, o, "togi-preflight-hw-sweep")
	started := time.Now()
	detail, err := New(o).Sweep(context.Background())
	elapsed := time.Since(started)
	if err != nil {
		t.Fatal(err)
	}
	if elapsed > teardownLimit {
		t.Fatalf("sweep exceeded shared deadline: %s", elapsed)
	}
	hardwareProcessesGone(t, orphan)
	hardwareScopesGone(t, "togi-trial-hw-orphan")
	for _, p := range []scopeProcess{unrelated, preflight} {
		alive, err := newOSHost().ProcessAlive(p)
		if err != nil || !alive || !newOSHost().InScope(p.PID, strings.TrimSuffix(p.Scope, ".scope")) {
			t.Fatalf("sweep changed unrelated scope: %+v alive=%t err=%v", p, alive, err)
		}
	}
	t.Logf("real orphan survived killed owner; %s in %s; unrelated and preflight scopes preserved", detail, elapsed)
}
