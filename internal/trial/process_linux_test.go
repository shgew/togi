//go:build linux

package trial

import (
	"context"
	"errors"
	"testing"
	"testing/synctest"
	"time"

	"github.com/shgew/togi/internal/machine"
)

func TestSystemdPreflightTimeout(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		started := time.Now()
		_, err := checkSystemdRun(func(ctx context.Context, name string, args ...string) ([]byte, error) {
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
		run := &running{host: h, options: Options{StopGrace: 3 * time.Second}, scopes: []string{"first", "second"}, instances: []*instance{{done: true}, {done: true}}}
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
