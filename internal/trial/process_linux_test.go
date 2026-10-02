//go:build linux

package trial

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"testing/synctest"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func TestSystemdPreflightTimeout(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		started := time.Now()
		_, err := checkSystemdRun(Identity{UID: 1001, GID: 1001}, func(ctx context.Context, name string, args ...string) ([]byte, error) {
			deadline, ok := ctx.Deadline()
			if !ok || deadline.Sub(started) != 30*time.Second {
				t.Fatalf("deadline = %v, present %v", deadline, ok)
			}
			<-ctx.Done()
			return nil, ctx.Err()
		})
		if !errors.Is(err, context.DeadlineExceeded) {
			t.Fatalf("preflight = %v", err)
		}
	})
}

type scopeCommandHost struct {
	*fakeHost
	os osHost
}

func (h scopeCommandHost) KillScope(ctx context.Context, scope string) ([]byte, error) {
	return h.os.KillScope(ctx, scope)
}

func TestScopeTimeoutUsesRemainingTeardownTime(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		started := time.Now()
		calls := 0
		h := scopeCommandHost{fakeHost: &fakeHost{}, os: osHost{command: func(ctx context.Context, name string, args ...string) ([]byte, error) {
			deadline, ok := ctx.Deadline()
			want := started.Add(2 * time.Second)
			if !ok || !deadline.Equal(want) {
				t.Fatalf("kill %d deadline = %v, want %v", calls, deadline, want)
			}
			calls++
			<-ctx.Done()
			return []byte("Unit not loaded; could not be found"), ctx.Err()
		}}}
		run := &running{host: h, options: Options{StopGrace: 3 * time.Second, SampleInterval: time.Second, teardown: teardownLimit}, scopes: []string{"first", "second"}}
		for range 2 {
			p, err := h.Start(context.Background(), []string{"exit"}, t.TempDir())
			if err != nil {
				t.Fatal(err)
			}
			run.instances = append(run.instances, &instance{PID: p.PID(), process: p, done: true})
		}
		synctest.Wait()
		err := run.teardown(&machine.Result{}, &recorder{})
		if !errors.Is(err, context.DeadlineExceeded) || !errors.Is(err, machine.ErrContainment) {
			t.Fatalf("cleanup = %v", err)
		}
		if calls != 2 {
			t.Fatalf("kills = %d", calls)
		}
	})
}

type blockedStartHost struct{ *fakeHost }

func (h blockedStartHost) Start(ctx context.Context, _ []string, _ string) (process, error) {
	<-ctx.Done()
	return nil, ctx.Err()
}

func TestTrialLaunchTimeout(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		o := fakeOptions(t, "work")
		o.NoScope = false
		r := New(o)
		r.host = blockedStartHost{&fakeHost{}}
		_, err := r.Start(context.Background(), testSpec("blocked-launch", machine.R1, time.Second))
		if !errors.Is(err, context.DeadlineExceeded) {
			t.Fatalf("launch = %v", err)
		}
	})
}

func TestRootLauncherClearsSupplementaryGroups(t *testing.T) {
	t.Parallel()
	attr := launcherAttributes(0, 42)
	if attr.Credential == nil {
		t.Fatal("root launcher inherits supplementary groups")
	}
	cred := attr.Credential
	if cred.Uid != 0 || cred.Gid != 42 || len(cred.Groups) != 0 || cred.NoSetGroups {
		t.Fatalf("launcher credentials = %+v; must retain root for the system bus and clear all supplementary groups", cred)
	}
	if !attr.Setpgid || attr.Pdeathsig != syscall.SIGKILL {
		t.Fatalf("launcher lost process ownership: %+v", attr)
	}
}

func TestUnprivilegedLauncherPreservesCredentials(t *testing.T) {
	t.Parallel()
	if attr := launcherAttributes(1001, 1002); attr.Credential != nil {
		t.Fatalf("unprivileged launcher tries privileged setgroups: %+v", attr.Credential)
	}
}

func TestProcStatDiagnostic(t *testing.T) {
	for _, tc := range []struct {
		name    string
		content string
		want    string
	}{
		{"missing delimiter", "123 (worker\n", `malformed proc stat PATH: "123 (worker\n"`},
		{"short", "123 (worker) R 1\n", `short proc stat PATH: "123 (worker) R 1\n"`},
		{"long malformed", strings.Repeat("x", 300), `malformed proc stat PATH: "` + strings.Repeat("x", 256) + `" (truncated)`},
		{"long short", "1 (w)" + strings.Repeat("x", 300), `short proc stat PATH: "1 (w)` + strings.Repeat("x", 251) + `" (truncated)`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "stat")
			if err := os.WriteFile(path, []byte(tc.content), 0o600); err != nil {
				t.Fatal(err)
			}
			_, err := procStat(path)
			if err == nil {
				t.Fatal("invalid proc stat accepted")
			}
			if diff := cmp.Diff(strings.ReplaceAll(tc.want, "PATH", path), err.Error()); diff != "" {
				t.Fatalf("diagnostic (-want +got):\n%s", diff)
			}
			if processDisappeared(err) {
				t.Fatal("malformed content reported as a disappeared process")
			}
		})
	}
}
