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
				if err != nil || info.Mode().Perm() != 0711 {
					t.Fatalf("trial parent must remain nonwritable by workload: %v, %v", info, err)
				}
			})
		})
	}
}

func TestScopeRefusesInaccessibleAncestor(t *testing.T) {
	o := fakeOptions(t, "exit")
	o.NoScope = false
	o.User.UID++
	private := filepath.Join(o.Dir, "private")
	if err := os.Mkdir(private, 0700); err != nil {
		t.Fatal(err)
	}
	o.Dir = filepath.Join(private, "trials")
	h := &fakeHost{inScope: true}
	r := New(o)
	r.host = h
	started, err := r.Start(context.Background(), testSpec("private", machine.R1, time.Second))
	if started != nil || err == nil || !strings.Contains(err.Error(), private) || len(h.procs) != 0 {
		t.Fatalf("inaccessible ancestor launched a backend: started=%v err=%v processes=%d", started, err, len(h.procs))
	}
	info, err := os.Stat(private)
	if err != nil || info.Mode().Perm() != 0700 {
		t.Fatalf("external ancestor permissions changed: info=%v err=%v", info, err)
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

type logSymlinkHost struct {
	*fakeHost
	test    *testing.T
	victims map[string]string
}

func (h *logSymlinkHost) Chown(path string, _, _ int) error {
	if filepath.Base(path) != "c01" {
		return nil
	}
	h.mu.Lock()
	started := len(h.procs)
	h.mu.Unlock()
	if started != 1 {
		h.test.Fatalf("symlink attack must run after c00 launched, got %d processes", started)
	}
	for name, victim := range h.victims {
		log := filepath.Join(path, name)
		if err := os.Rename(log, log+".retained"); err != nil && !os.IsNotExist(err) {
			return err
		}
		if err := os.Symlink(victim, log); err != nil {
			return err
		}
	}
	return nil
}

func TestScopeLogSymlinkRace(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		o := fakeOptions(t, "work")
		o.NoScope = false
		victims := map[string]string{}
		for _, name := range []string{"stdout.log", "stderr.log"} {
			victims[name] = filepath.Join(t.TempDir(), "events.jsonl")
			if err := os.WriteFile(victims[name], []byte("protected journal\n"), 0600); err != nil {
				t.Fatal(err)
			}
		}
		h := &logSymlinkHost{fakeHost: &fakeHost{inScope: true}, test: t, victims: victims}
		r := New(o)
		r.host = h
		spec := testSpec("log-race", machine.R7, 500*time.Millisecond)
		spec.Cores, spec.CPUs = []int{0, 1}, []int{0, 1}
		started, err := r.Start(context.Background(), spec)
		if err != nil {
			t.Fatal(err)
		}
		result, err := started.Wait(context.Background(), &recorder{})
		if err != nil || result.Signal != "" || result.Inconclusive != "" {
			t.Fatalf("trial result = %+v, err = %v", result, err)
		}
		for name, victim := range victims {
			data, err := os.ReadFile(victim)
			if err != nil || string(data) != "protected journal\n" {
				t.Errorf("%s symlink changed victim: %q, %v", name, data, err)
			}
		}
		data, err := os.ReadFile(filepath.Join(o.Dir, spec.ID, "c01", "stdout.log.retained"))
		if err != nil || !strings.Contains(string(data), "progress 1\n") {
			t.Fatalf("captured stdout descriptor was not retained: %q, %v", data, err)
		}
		t.Log("c00 launched before c01 replaced both log paths with symlinks; victims unchanged and stdout captured through its original descriptor")
	})
}

func TestReusedTrialDirectoryDoesNotFollowSymlinks(t *testing.T) {
	t.Parallel()
	for _, child := range []string{"directory", "symlink"} {
		t.Run(child, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				o := fakeOptions(t, "exit")
				o.NoScope = false
				o.Backends[machine.Mprime] = inputBackend{fakeBackend{"exit"}}
				spec := testSpec("0001", machine.R1, time.Second)
				root := filepath.Join(o.Dir, spec.ID)
				dir := filepath.Join(root, "work")
				if err := os.MkdirAll(root, 0755); err != nil {
					t.Fatal(err)
				}
				if child == "symlink" {
					target := t.TempDir()
					if err := os.Symlink(target, dir); err != nil {
						t.Fatal(err)
					}
					dir = target
				} else if err := os.Mkdir(dir, 0777); err != nil {
					t.Fatal(err)
				}
				victim := filepath.Join(t.TempDir(), "control")
				if err := os.WriteFile(victim, []byte("protected control\n"), 0600); err != nil {
					t.Fatal(err)
				}
				for _, name := range []string{"prime.txt", "local.txt", "stdout.log", "stderr.log"} {
					if err := os.Symlink(victim, filepath.Join(dir, name)); err != nil {
						t.Fatal(err)
					}
				}
				r := New(o)
				r.host = &fakeHost{inScope: true}
				started, err := r.Start(context.Background(), spec)
				if err != nil {
					t.Fatal(err)
				}
				if err := started.(*running).Stop(); err != nil {
					t.Fatal(err)
				}
				data, err := os.ReadFile(victim)
				if err != nil || string(data) != "protected control\n" {
					t.Fatalf("reused %s changed victim: %q, %v", child, data, err)
				}
				for _, name := range []string{"prime.txt", "local.txt"} {
					data, err := os.ReadFile(filepath.Join(root, "work", name))
					if err != nil || string(data) != "input" {
						t.Fatalf("fresh %s = %q, %v", name, data, err)
					}
				}
				t.Logf("reused trial with attacker-controlled %s and input/log symlinks prepared fresh inputs; victim unchanged", child)
			})
		})
	}
}

type scopeDirectoryHost struct{ *fakeHost }

func (h scopeDirectoryHost) Start(ctx context.Context, argv []string, dir string) (process, error) {
	scopeDir := argv[slices.Index(argv, "--working-directory")+1]
	if !filepath.IsAbs(scopeDir) {
		scopeDir = filepath.Join(dir, scopeDir)
	}
	data, err := os.ReadFile(filepath.Join(scopeDir, "prime.txt"))
	if err != nil {
		return nil, err
	}
	if string(data) != "input" {
		return nil, errors.New("scope did not reach prepared input")
	}
	return h.fakeHost.Start(ctx, argv, dir)
}

func TestRelativeTrialDirectory(t *testing.T) {
	dir := t.TempDir()
	for _, path := range []string{filepath.Dir(dir), dir} {
		if err := os.Chmod(path, 0711); err != nil {
			t.Fatal(err)
		}
	}
	t.Chdir(dir)
	synctest.Test(t, func(t *testing.T) {
		o := fakeOptions(t, "exit")
		o.Dir, o.NoScope = "trials", false
		o.Backends[machine.Mprime] = inputBackend{fakeBackend{"exit"}}
		r := New(o)
		r.host = scopeDirectoryHost{&fakeHost{inScope: true}}
		started, err := r.Start(context.Background(), testSpec("relative", machine.R1, time.Second))
		if err != nil {
			t.Fatal(err)
		}
		if err := started.(*running).Stop(); err != nil {
			t.Fatal(err)
		}
		t.Log("relative trials directory reached prepared inputs after launcher and scope working-directory resolution")
	})
}

func TestInputOwnershipFailureNamesInput(t *testing.T) {
	failure := errors.New("input ownership refused")
	dir := t.TempDir()
	var paths []string
	err := ownDirectory(dir, []string{"prime.txt", "local.txt"}, Identity{UID: 1, GID: 1}, func(path string, _, _ int) error {
		paths = append(paths, path)
		return failure
	})
	if !errors.Is(err, failure) || !strings.Contains(err.Error(), filepath.Join(dir, "prime.txt")) {
		t.Fatalf("input ownership error: %v", err)
	}
	if diff := cmp.Diff([]string{filepath.Join(dir, "prime.txt")}, paths); diff != "" {
		t.Fatal(diff)
	}
}
