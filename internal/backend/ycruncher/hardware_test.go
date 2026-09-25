//go:build hardware && linux

package ycruncher_test

import (
	"context"
	"os"
	"strconv"
	"testing"
	"time"

	"code.marleb.org/shgew/shycler/internal/backend"
	"code.marleb.org/shgew/shycler/internal/backend/ycruncher"
	"code.marleb.org/shgew/shycler/internal/config"
	"code.marleb.org/shgew/shycler/internal/machine"
	"code.marleb.org/shgew/shycler/internal/trial"
)

type logReporter struct{ t *testing.T }

func (r logReporter) Progress(s string)       { r.t.Log("progress:", s) }
func (r logReporter) Sample(s machine.Sample) { r.t.Logf("sample: %+v", s) }

func packagePath(t *testing.T) string {
	t.Helper()
	if p := os.Getenv("SHYCLER_YCRUNCHER"); p != "" {
		return p
	}
	cfg, err := config.Load(config.DefaultPath)
	if err == nil && cfg.Backends.Ycruncher != "" {
		return cfg.Backends.Ycruncher
	}
	t.Fatalf("set SHYCLER_YCRUNCHER, or backends.ycruncher in %s (%v)", config.DefaultPath, err)
	return ""
}

func TestHardwareWorkloads(t *testing.T) {
	b := ycruncher.New(packagePath(t))
	detail, err := b.Check()
	if err != nil {
		t.Fatal(err)
	}
	t.Log(detail)
	r := trial.New(trial.Options{
		Dir:      t.TempDir(),
		Backends: map[machine.Backend]backend.Backend{machine.Ycruncher: b},
		Cores:    []machine.CoreInfo{{Core: 2, CPUs: []int{2}}},
	})
	for i, w := range append(machine.Workloads(machine.R1), machine.Workloads(machine.R2)...) {
		if w.Backend != machine.Ycruncher {
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
