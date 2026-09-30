//go:build hardware && linux

package mprime_test

import (
	"context"
	"os"
	"path/filepath"
	"strconv"
	"testing"
	"time"

	"github.com/shgew/togi/internal/backend"
	"github.com/shgew/togi/internal/backend/mprime"
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

func packagePath(t *testing.T) string {
	t.Helper()
	if p := os.Getenv("TOGI_MPRIME"); p != "" {
		return p
	}
	cfg, err := config.Load(config.DefaultPath)
	if err == nil && cfg.Backends.Mprime != "" {
		return cfg.Backends.Mprime
	}
	t.Fatalf("set TOGI_MPRIME, or backends.mprime in %s (%v)", config.DefaultPath, err)
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
	b := mprime.New(packagePath(t))
	detail, err := b.Check()
	if err != nil {
		t.Fatal(err)
	}
	t.Log(detail)
	r := trial.New(trial.Options{
		Dir:      hardwareDir(t),
		User:     hardwareUser(t),
		Backends: map[machine.Backend]backend.Backend{machine.Mprime: b},
		Cores:    []machine.CoreInfo{{Core: 2, CPUs: []int{2}}},
	})
	for i, w := range append(machine.Workloads(machine.R1), machine.Workloads(machine.R2)...) {
		if w.Backend != machine.Mprime {
			continue
		}
		t.Run(w.ID, func(t *testing.T) {
			spec := machine.TrialSpec{ID: "hw" + w.ID, Regime: machine.R1, Workload: w, Condition: machine.Isolated, Cores: []int{2}, CPUs: []int{2}, Duration: 20 * time.Second, Index: i}
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
			t.Logf("ran %s, Tctl max %s", res.Ran, tctl)
			if res.Signal != "" || res.Inconclusive != "" || len(res.Escaped) > 0 || res.Ran < spec.Duration {
				t.Fatalf("%s did not pass: %+v", w.ID, res)
			}
		})
	}
}
