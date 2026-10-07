//go:build hardware && linux

package backend_test

import (
	"context"
	"os"
	"path/filepath"
	"strconv"
	"testing"
	"time"

	"github.com/shgew/togi/internal/backend"
	"github.com/shgew/togi/internal/backend/mprime"
	"github.com/shgew/togi/internal/backend/ycruncher"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/hostlock"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/trial"
)

type logReporter struct{ t *testing.T }

func (r logReporter) Progress(s string)       { r.t.Log("progress:", s) }
func (r logReporter) Sample(s machine.Sample) { r.t.Logf("sample: %+v", s) }
func (r logReporter) Signal(core int, signal machine.Signal, detail string) {
	r.t.Logf("signal: core %02d %s: %s", core, signal, detail)
}

func packagePath(t *testing.T, env, key string, configured func(config.Config) string) string {
	t.Helper()
	if p := os.Getenv(env); p != "" {
		return p
	}
	cfg, err := config.Load(config.DefaultPath)
	if err == nil && configured(cfg) != "" {
		return configured(cfg)
	}
	t.Fatalf("set %s, or backends.%s in %s (%v)", env, key, config.DefaultPath, err)
	return ""
}

func hardwareUser(t *testing.T) trial.Identity {
	t.Helper()
	if os.Geteuid() != 0 {
		return trial.Identity{UID: uint32(os.Geteuid()), GID: uint32(os.Getegid())}
	}
	cfg, err := config.Load(config.DefaultPath)
	if err != nil {
		t.Fatal(err)
	}
	id, err := trial.LookupIdentity(cfg.BackendUser)
	if err != nil {
		t.Fatal(err)
	}
	return id
}

func hardwareDir(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	for _, path := range []string{filepath.Dir(dir), dir} {
		if err := os.Chmod(path, 0755); err != nil {
			t.Fatal(err)
		}
	}
	return dir
}

func TestHardwareWorkloads(t *testing.T) {
	lock, err := hostlock.Acquire(hostlock.Path)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := lock.Close(); err != nil {
			t.Error(err)
		}
	})
	for _, tc := range []struct {
		backend machine.Backend
		env     string
		new     func(pkg string) backend.Backend
		path    func(config.Config) string
	}{
		{machine.Mprime, "TOGI_MPRIME", func(pkg string) backend.Backend { return mprime.New(pkg) }, func(c config.Config) string { return c.Backends.Mprime }},
		{machine.Ycruncher, "TOGI_YCRUNCHER", func(pkg string) backend.Backend { return ycruncher.New(pkg) }, func(c config.Config) string { return c.Backends.Ycruncher }},
	} {
		t.Run(string(tc.backend), func(t *testing.T) {
			b := tc.new(packagePath(t, tc.env, string(tc.backend), tc.path))
			detail, err := b.Check()
			if err != nil {
				t.Fatal(err)
			}
			t.Log(detail)
			r := trial.New(trial.Options{
				Dir:      hardwareDir(t),
				User:     hardwareUser(t),
				Backends: map[machine.Backend]backend.Backend{tc.backend: b},
				Cores:    []machine.CoreInfo{{Core: 2, CPUs: []int{2}}},
			})
			for _, regime := range []machine.Regime{machine.R1, machine.R2} {
				for i, w := range machine.Workloads(regime) {
					if w.Backend != tc.backend {
						continue
					}
					t.Run(w.ID, func(t *testing.T) {
						spec := machine.TrialSpec{ID: "hw" + w.ID, Regime: regime, Workload: w, Condition: machine.Alone, Cores: []int{2}, CPUs: []int{2}, Duration: 20 * time.Second, Index: i}
						running, err := r.Start(context.Background(), spec)
						if err != nil {
							t.Fatal(err)
						}
						res, err := running.Wait(context.Background(), logReporter{t})
						if err != nil {
							t.Fatal(err)
						}
						tctl := "none"
						if res.TctlMaxC != nil {
							tctl = strconv.Itoa(*res.TctlMaxC) + " C"
						}
						t.Logf("%s ran %s, Tctl max %s", regime, res.Ran, tctl)
						if res.Signal != "" || res.Inconclusive != "" || len(res.Escaped) > 0 || res.Ran < spec.Duration {
							t.Fatalf("%s did not pass: %+v", w.ID, res)
						}
					})
				}
			}
		})
	}
}
