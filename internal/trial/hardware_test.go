//go:build hardware

package trial

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"

	"code.marleb.org/shgew/shycler/internal/backend"
	"code.marleb.org/shgew/shycler/internal/machine"
)

func TestHardwareScope(t *testing.T) {
	detail, err := CheckSystemdRun()
	if err != nil {
		t.Fatal(err)
	}
	t.Log(detail)
	r := New(Options{
		Dir:      t.TempDir(),
		Backends: map[machine.Backend]backend.Backend{machine.Mprime: helperBackend{"escape:3"}},
		Cores:    []machine.CoreInfo{{Core: 2, CPUs: []int{2}}},
	})
	spec := machine.TrialSpec{ID: "hw01", Regime: machine.R1, Workload: machine.Workload{Backend: machine.Mprime}, Cores: []int{2}, CPUs: []int{2}, Duration: 3 * time.Second}
	running, err := r.Start(context.Background(), spec)
	if err != nil {
		t.Fatal(err)
	}
	var rec recorder
	res, err := running.Wait(context.Background(), &rec)
	if err != nil {
		t.Fatal(err)
	}
	t.Logf("started %+v", running.Started())
	t.Logf("result %+v, progress %q, samples %+v", res, rec.progress, rec.samples)
	if len(res.Escaped) > 0 {
		t.Fatalf("the scope let the helper escape to %v", res.Escaped)
	}
	if !strings.Contains(res.Inconclusive, "PIN FAILED") {
		t.Fatalf("the helper's pin to cpu 3 was not refused: %+v", res)
	}
	args := []string{"show", "shycler-trial-hw01.scope", "-p", "ActiveState", "--value"}
	if os.Geteuid() != 0 {
		args = append([]string{"--user"}, args...)
	}
	// systemd notices the emptied cgroup asynchronously, so the scope may stay active for a moment after the exit.
	var state string
	for range 50 {
		out, err := exec.Command("systemctl", args...).Output()
		if err != nil {
			t.Fatal(err)
		}
		if state = strings.TrimSpace(string(out)); state != "active" {
			t.Logf("scope %s after the trial", state)
			return
		}
		time.Sleep(100 * time.Millisecond)
	}
	t.Fatalf("scope still %s 5s after the trial", state)
}

func TestHardwareScopeKillsDetachedDescendant(t *testing.T) {
	if _, err := CheckSystemdRun(); err != nil {
		t.Fatal(err)
	}
	cpu := testCPUs(t)[0]
	o := Options{
		Dir:      t.TempDir(),
		Backends: map[machine.Backend]backend.Backend{machine.Mprime: helperBackend{"descendant"}},
		Cores:    []machine.CoreInfo{{Core: 0, CPUs: []int{cpu}}},
	}
	spec := machine.TrialSpec{ID: "hw-descendant", Regime: machine.R1, Workload: machine.Workload{Backend: machine.Mprime}, Cores: []int{0}, CPUs: []int{cpu}, Duration: time.Second}
	r, err := New(o).Start(context.Background(), spec)
	if err != nil {
		t.Fatal(err)
	}
	var rec recorder
	result, err := r.Wait(context.Background(), &rec)
	if err != nil {
		t.Fatal(err)
	}
	pidText, err := os.ReadFile(filepath.Join(o.Dir, spec.ID, "descendant.pid"))
	if err != nil {
		t.Fatal(err)
	}
	pid, err := strconv.Atoi(string(pidText))
	if err != nil {
		t.Fatal(err)
	}
	defer syscall.Kill(pid, syscall.SIGKILL)
	for range 50 {
		fields, err := procStat("/proc/" + strconv.Itoa(pid) + "/stat")
		if os.IsNotExist(err) || (err == nil && fields[0] == "Z") {
			return
		}
		if err != nil {
			t.Fatal(err)
		}
		time.Sleep(100 * time.Millisecond)
	}
	t.Fatalf("descendant %d survived teardown: %+v", pid, result)
}
