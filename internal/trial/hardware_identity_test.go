//go:build hardware && linux

package trial

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/shgew/togi/internal/backend"
	"github.com/shgew/togi/internal/machine"
)

type hardwareIdentityBackend struct{ helperBackend }

func (h hardwareIdentityBackend) Prepare(w machine.Workload, dir string, cpus []int) (backend.Launch, error) {
	for _, path := range []string{filepath.Join(dir, "input.txt"), filepath.Join(filepath.Dir(dir), "retained")} {
		if err := os.WriteFile(path, []byte("prepared input\n"), 0644); err != nil {
			return backend.Launch{}, err
		}
	}
	launch, err := h.helperBackend.Prepare(w, dir, cpus)
	launch.Files = []string{"input.txt"}
	return launch, err
}

func hardwareReadIdentity(t *testing.T, dir string) helperIdentityReport {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for {
		data, err := os.ReadFile(filepath.Join(dir, "identity.json"))
		if err == nil {
			var report helperIdentityReport
			if err := json.Unmarshal(data, &report); err != nil {
				t.Fatal(err)
			}
			return report
		}
		if !os.IsNotExist(err) || !time.Now().Before(deadline) {
			t.Fatalf("wait for backend identity report: %v", err)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func hardwareOwner(t *testing.T, path string, uid, gid uint32) {
	t.Helper()
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	stat := info.Sys().(*syscall.Stat_t)
	if stat.Uid != uid || stat.Gid != gid {
		t.Fatalf("ownership of %s = %d:%d, want %d:%d", path, stat.Uid, stat.Gid, uid, gid)
	}
}

func TestHardwareScopeBackendIdentity(t *testing.T) {
	user := hardwareRoot(t)
	o := hardwareOptions(t, user, "backend-identity")
	o.Backends[machine.Mprime] = hardwareIdentityBackend{o.Backends[machine.Mprime].(helperBackend)}
	for _, name := range []string{"control", "events.jsonl"} {
		if err := os.WriteFile(filepath.Join(o.Dir, name), []byte("protected parent\n"), 0644); err != nil {
			t.Fatal(err)
		}
	}
	spec := testSpec("hw-identity", machine.R1, time.Minute)
	spec.CPUs = o.Cores[0].CPUs
	scope := "togi-trial-" + spec.ID
	hardwareScopeCleanup(t, scope)
	started, err := New(o).Start(context.Background(), spec)
	if err != nil {
		t.Fatal(err)
	}
	trial := started.(*running)
	t.Cleanup(func() { _ = trial.Stop() })
	work := filepath.Join(o.Dir, spec.ID, "work")
	report := hardwareReadIdentity(t, work)
	if report.UID != int(user.UID) || report.GID != int(user.GID) || len(report.Groups) != 0 || report.Dir != work {
		t.Fatalf("privileged scope did not use configured backend identity and prepared cwd: %+v, want %d:%d at %s with no supplementary groups", report, user.UID, user.GID, work)
	}
	p := trial.instances[0].process.(*execProcess)
	identity := hardwareIdentity(t, p.PID(), scope)
	for _, path := range []string{"created.txt", "input.txt"} {
		if report.Writes[path] != "written" {
			t.Fatalf("instance write %s: %q", path, report.Writes[path])
		}
		data, err := os.ReadFile(filepath.Join(work, path))
		if err != nil || !strings.HasSuffix(string(data), "backend write\n") {
			t.Fatalf("backend write %s not retained: %q err=%v", path, data, err)
		}
		hardwareOwner(t, filepath.Join(work, path), user.UID, user.GID)
	}
	for _, path := range []string{"stdout.log", "stderr.log", "../passed", "../retained", "../../control", "../../events.jsonl", "../../outside"} {
		if report.Writes[path] != "denied" {
			t.Fatalf("protected path %s accepted backend write: %q", path, report.Writes[path])
		}
	}
	for _, path := range []string{o.Dir, filepath.Join(o.Dir, spec.ID), filepath.Join(work, "stdout.log"), filepath.Join(work, "stderr.log"), filepath.Join(o.Dir, spec.ID, "retained"), filepath.Join(o.Dir, "control"), filepath.Join(o.Dir, "events.jsonl")} {
		hardwareOwner(t, path, 0, uint32(os.Getegid()))
	}
	hardwareOwner(t, work, user.UID, user.GID)
	for _, path := range []string{filepath.Join(o.Dir, spec.ID, "passed"), filepath.Join(o.Dir, "outside")} {
		if _, err := os.Stat(path); !os.IsNotExist(err) {
			t.Fatalf("backend created protected parent file %s: %v", path, err)
		}
	}
	for _, name := range []string{"control", "events.jsonl"} {
		data, err := os.ReadFile(filepath.Join(o.Dir, name))
		if err != nil || string(data) != "protected parent\n" {
			t.Fatalf("protected parent file changed: %s=%q err=%v", name, data, err)
		}
	}
	begin := time.Now()
	if err := trial.Stop(); err != nil {
		t.Fatal(err)
	}
	if elapsed := time.Since(begin); elapsed > teardownLimit {
		t.Fatalf("identity scope cleanup exceeded shared deadline: %s", elapsed)
	}
	hardwareProcessesGone(t, identity)
	hardwareScopesGone(t, scope)
	t.Logf("real root-launched backend reports UID:GID %d:%d, empty supplementary groups and cwd %s; instance/input writes succeed; root-owned parent/log/control writes denied", report.UID, report.GID, report.Dir)
}
