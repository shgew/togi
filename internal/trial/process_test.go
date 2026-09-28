//go:build integration && linux

package trial

import (
	"context"
	"fmt"
	"runtime"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/shgew/togi/internal/machine"
)

func TestProcessTrials(t *testing.T) {
	for _, tt := range []struct {
		name, mode string
		duration   time.Duration
		signal     machine.Signal
	}{
		{"pass", "work", 600 * time.Millisecond, ""},
		{"error", "error", time.Second, machine.ComputationError},
		{"exit", "exit", time.Second, machine.UnexpectedExit},
	} {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			o := testOptions(t, tt.mode)
			spec := testSpec(tt.name, machine.R1, tt.duration)
			spec.CPUs = []int{o.Cores[0].CPUs[0]}
			running, err := New(o).Start(context.Background(), spec)
			if err != nil {
				t.Fatal(err)
			}
			rec := &recorder{}
			result, err := running.Wait(context.Background(), rec)
			if err != nil || result.Signal != tt.signal || len(result.Escaped) != 0 {
				t.Fatalf("result %+v err %v", result, err)
			}
			if tt.name == "pass" && len(rec.progress) == 0 {
				t.Fatal("no progress captured")
			}
		})
	}
}

func TestProcessEscape(t *testing.T) {
	o := testOptions(t, "")
	o.Backends[machine.Mprime] = helperBackend{mode: fmt.Sprintf("escape:%d", o.Cores[1].CPUs[0])}
	spec := testSpec("escape", machine.R1, time.Second)
	spec.CPUs = []int{o.Cores[0].CPUs[0]}
	running, err := New(o).Start(context.Background(), spec)
	if err != nil {
		t.Fatal(err)
	}
	rec := &recorder{}
	result, err := running.Wait(context.Background(), rec)
	if err != nil {
		t.Fatal(err)
	}
	if result.Inconclusive != "" && strings.Contains(result.Inconclusive, "PIN FAILED") {
		t.Skip("pinning unavailable")
	}
	if !slices.Equal(result.Escaped, []int{o.Cores[1].CPUs[0]}) || len(rec.samples) == 0 || rec.samples[0].Warning != "outside allowed cpus" {
		t.Fatalf("escape result %+v samples %+v", result, rec.samples)
	}
}

func TestProcessR6StartsStopped(t *testing.T) {
	o := testOptions(t, "work")
	o.SampleInterval = 450 * time.Millisecond
	spec := testSpec("r6-idle", machine.R6, 600*time.Millisecond)
	spec.Cores = []int{0, 1}
	spec.CPUs = []int{o.Cores[0].CPUs[0], o.Cores[1].CPUs[0]}
	r, err := New(o).Start(context.Background(), spec)
	if err != nil {
		t.Fatal(err)
	}
	trial := r.(*running)
	for _, inst := range trial.instances {
		deadline := time.Now().Add(time.Second)
		for {
			fields, err := procStat(fmt.Sprintf("/proc/%d/stat", inst.PID))
			if err == nil && fields[0] == "T" {
				break
			}
			if time.Now().After(deadline) {
				trial.abort()
				t.Fatalf("core %02d not initially stopped: state %v, err %v", inst.Core, fields, err)
			}
			runtime.Gosched()
		}
	}
	var rec recorder
	result, err := trial.Wait(context.Background(), &rec)
	if err != nil || result.Signal != "" || result.Stops != 3 || result.Conts != 1 {
		t.Fatalf("R6 initial stops and bursts result %+v, err %v", result, err)
	}
}
