package trial

import (
	"context"
	"errors"
	"os"
	"os/user"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/backend"
	"github.com/shgew/togi/internal/machine"
)

func TestLookupIdentity(t *testing.T) {
	t.Parallel()
	for _, tt := range []struct {
		name, uid, gid, wantErr string
		missing                 bool
	}{
		{name: "togi-trial", uid: "997", gid: "996"},
		{name: "", wantErr: "not configured"},
		{name: "missing", missing: true, wantErr: "unknown user"},
		{name: "root", uid: "0", gid: "0", wantErr: "non-root"},
		{name: "root-alias", uid: "0", gid: "996", wantErr: "non-root"},
		{name: "root-group", uid: "997", gid: "0", wantErr: "non-root"},
		{name: "invalid-uid", uid: "bad", gid: "996", wantErr: "uid"},
		{name: "invalid-gid", uid: "997", gid: "-1", wantErr: "gid"},
		{name: "overflow", uid: "4294967296", gid: "996", wantErr: "uid"},
		{name: "unchanged-uid", uid: "4294967295", gid: "996", wantErr: "4294967294"},
		{name: "unchanged-gid", uid: "997", gid: "4294967295", wantErr: "4294967294"},
	} {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			got, err := lookupIdentity(tt.name, func(name string) (*user.User, error) {
				if tt.name == "" {
					t.Fatal("empty backend_user must not resolve a default identity")
				}
				if tt.missing {
					return nil, user.UnknownUserError(name)
				}
				return &user.User{Uid: tt.uid, Gid: tt.gid}, nil
			})
			if tt.wantErr != "" {
				if err == nil || !strings.Contains(err.Error(), tt.wantErr) || got != (Identity{}) {
					t.Fatalf("identity = %+v, err = %v; want refusal containing %q", got, err, tt.wantErr)
				}
				return
			}
			if err != nil || got != (Identity{UID: 997, GID: 996}) {
				t.Fatalf("identity = %+v, err = %v", got, err)
			}
		})
	}
}

func TestScopeArgv(t *testing.T) {
	t.Parallel()
	want := []string{"systemd-run"}
	if os.Geteuid() != 0 {
		want = append(want, "--user")
	}
	want = append(want, "--scope", "--quiet", "--collect", "--unit", "togi-trial-7", "--uid", "997", "--gid", "996", "--working-directory", "/trials/7/work", "-p", "AllowedCPUs=2,18", "-p", "DefaultDependencies=no", "--", "/package/bin/mprime", "-t")
	got := scopeArgv("togi-trial-7", []int{2, 18}, Identity{UID: 997, GID: 996}, "/trials/7/work", "/package/bin/mprime", "-t")
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("scope argv (-want +got):\n%s", diff)
	}
}

type inputBackend struct{ fakeBackend }

func (b inputBackend) Prepare(w machine.Workload, dir string, cpus []int) (backend.Launch, error) {
	launch, err := b.fakeBackend.Prepare(w, dir, cpus)
	if err != nil {
		return launch, err
	}
	launch.Files = []string{"prime.txt", "local.txt"}
	for _, name := range launch.Files {
		if err := os.WriteFile(filepath.Join(dir, name), []byte("input"), 0644); err != nil {
			return backend.Launch{}, err
		}
	}
	return launch, nil
}

type ownershipHost struct {
	*fakeHost
	owned map[string]Identity
	fail  error
}

func (h *ownershipHost) Chown(path string, uid, gid int) error {
	if h.fail != nil {
		return h.fail
	}
	h.owned[path] = Identity{UID: uint32(uid), GID: uint32(gid)}
	return nil
}

func TestScopeInstanceOwnership(t *testing.T) {
	for _, regime := range []machine.Regime{machine.R1, machine.R7} {
		t.Run(string(regime), func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				o := fakeOptions(t, "exit")
				o.NoScope = false
				o.Backends[machine.Mprime] = inputBackend{fakeBackend{"exit"}}
				h := &ownershipHost{fakeHost: &fakeHost{inScope: true}, owned: map[string]Identity{}}
				r := New(o)
				r.host = h
				spec := testSpec("ownership", regime, time.Second)
				prefixes := []string{"work"}
				if regime == machine.R7 {
					spec.Cores, spec.CPUs = []int{0, 1}, []int{0, 1}
					prefixes = []string{"c00", "c01"}
				}
				started, err := r.Start(context.Background(), spec)
				if err != nil {
					t.Fatal(err)
				}
				if err := started.(*running).Stop(); err != nil {
					t.Fatal(err)
				}
				want := map[string]Identity{}
				var files []string
				for _, prefix := range prefixes {
					dir := filepath.Join(o.Dir, spec.ID, prefix)
					want[dir] = o.User
					for _, name := range []string{"prime.txt", "local.txt"} {
						want[filepath.Join(dir, name)] = o.User
						files = append(files, filepath.Join(prefix, name))
					}
				}
				if diff := cmp.Diff(want, h.owned); diff != "" {
					t.Fatalf("only instance directories and inputs change ownership (-want +got):\n%s", diff)
				}
				if !slices.Equal(files, started.Started().Files) {
					t.Fatalf("input paths = %v, want %v", started.Started().Files, files)
				}
				info, err := os.Stat(filepath.Join(o.Dir, spec.ID))
				if err != nil || info.Mode().Perm() != 0755 {
					t.Fatalf("trial parent must remain nonwritable by workload: %v, %v", info, err)
				}
			})
		})
	}
}

func TestScopeRefusesIdentityBeforePreparing(t *testing.T) {
	t.Parallel()
	o := fakeOptions(t, "exit")
	o.NoScope = false
	o.User = Identity{}
	r := New(o)
	h := &fakeHost{}
	r.host = h
	_, err := r.Start(context.Background(), testSpec("refused", machine.R1, time.Second))
	if err == nil || !strings.Contains(err.Error(), "backend_user") {
		t.Fatalf("missing identity = %v", err)
	}
	if _, err := os.Stat(filepath.Join(o.Dir, "refused")); !os.IsNotExist(err) || len(h.procs) != 0 {
		t.Fatalf("refused identity prepared or launched a workload: stat=%v processes=%d", err, len(h.procs))
	}
}

func TestOwnershipFailureDoesNotLaunch(t *testing.T) {
	t.Parallel()
	o := fakeOptions(t, "exit")
	o.NoScope = false
	failure := errors.New("ownership refused")
	h := &ownershipHost{fakeHost: &fakeHost{}, fail: failure}
	r := New(o)
	r.host = h
	_, err := r.Start(context.Background(), testSpec("ownership-failed", machine.R1, time.Second))
	if !errors.Is(err, failure) || len(h.procs) != 0 {
		t.Fatalf("ownership error = %v, processes = %d", err, len(h.procs))
	}
}
