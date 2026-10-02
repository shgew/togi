package sim

import (
	"errors"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func TestCrashWorkerSamplesSurviveRecovery(t *testing.T) {
	t.Parallel()
	cfg := Config{Cores: 2, Script: map[string]Outcome{"0001": {Core: 1, Signal: machine.Crash, AtS: 6}}}
	m := newMachine(t, cfg)
	dir := t.TempDir()
	m.SetSamplesDir(dir)
	_, err := runSpec(t, m, "0001", machine.R7, machine.PickWorkload(machine.R7, 0), []int{0, 1}, 10*time.Second, nil)
	if !errors.Is(err, machine.ErrCrashed) {
		t.Fatalf("crash: %v", err)
	}
	m.Reboot()
	samples := slices.Collect(m.Seams().Trials.Samples("0001"))
	want := []machine.TrialConditions{
		{ElapsedMS: 1000, WorkerCPUMS: map[int]int64{0: 1000, 1: 1000}},
		{ElapsedMS: 2000, WorkerCPUMS: map[int]int64{0: 2000, 1: 2000}},
		{ElapsedMS: 3000, WorkerCPUMS: map[int]int64{0: 3000, 1: 3000}},
		{ElapsedMS: 4000, WorkerCPUMS: map[int]int64{0: 4000, 1: 4000}},
		{ElapsedMS: 5000, WorkerCPUMS: map[int]int64{0: 5000, 1: 4000}},
	}
	if diff := cmp.Diff(want, samples); diff != "" {
		t.Fatal(diff)
	}
	recovered := newMachine(t, cfg)
	recovered.SetSamplesDir(dir)
	if diff := cmp.Diff(want, slices.Collect(recovered.Seams().Trials.Samples("0001"))); diff != "" {
		t.Fatal(diff)
	}
}

func TestSimulatedSamples(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name        string
		ran         time.Duration
		stalledCore int
		want        []machine.TrialConditions
	}{
		{"before first tick", 500 * time.Millisecond, -1, nil},
		{"at first tick", time.Second, -1, nil},
		{"between ticks", 2500 * time.Millisecond, -1, []machine.TrialConditions{
			{ElapsedMS: 1000, WorkerCPUMS: map[int]int64{2: 2000, 7: 2000}},
			{ElapsedMS: 2000, WorkerCPUMS: map[int]int64{2: 4000, 7: 4000}},
		}},
		{"short crash", 2500 * time.Millisecond, 7, []machine.TrialConditions{
			{ElapsedMS: 1000, WorkerCPUMS: map[int]int64{2: 2000, 7: 2000}},
			{ElapsedMS: 2000, WorkerCPUMS: map[int]int64{2: 4000, 7: 2000}},
		}},
		{"fractional crash", 4500 * time.Millisecond, 7, []machine.TrialConditions{
			{ElapsedMS: 1000, WorkerCPUMS: map[int]int64{2: 2000, 7: 2000}},
			{ElapsedMS: 2000, WorkerCPUMS: map[int]int64{2: 4000, 7: 4000}},
			{ElapsedMS: 3000, WorkerCPUMS: map[int]int64{2: 6000, 7: 5000}},
			{ElapsedMS: 4000, WorkerCPUMS: map[int]int64{2: 8000, 7: 5000}},
		}},
		{"idle culprit", 3 * time.Second, 0, []machine.TrialConditions{
			{ElapsedMS: 1000, WorkerCPUMS: map[int]int64{2: 2000, 7: 2000}},
			{ElapsedMS: 2000, WorkerCPUMS: map[int]int64{2: 4000, 7: 4000}},
		}},
	} {
		for _, fileBacked := range []bool{false, true} {
			name := tc.name + "/memory"
			if fileBacked {
				name = tc.name + "/file"
			}
			t.Run(name, func(t *testing.T) {
				t.Parallel()
				m := newMachine(t, Config{Cores: 8})
				if fileBacked {
					m.SetSamplesDir(t.TempDir())
				}
				cores := []int{2, 7}
				r := running{m: m, spec: machine.TrialSpec{ID: "0001", Cores: cores, Workload: machine.Workload{Threads: 2}}}
				if err := r.sampleConditions(tc.ran, tc.stalledCore); err != nil {
					t.Fatal(err)
				}
				cores[0] = 0
				m.Reboot()
				samples := m.Seams().Trials.Samples("0001")
				for sample := range samples {
					if len(tc.want) == 0 {
						t.Fatalf("unexpected sample before the first tick: %+v", sample)
					}
					if diff := cmp.Diff(tc.want[0], sample); diff != "" {
						t.Fatal(diff)
					}
					break
				}
				for range 2 {
					if diff := cmp.Diff(tc.want, slices.Collect(samples)); diff != "" {
						t.Fatal(diff)
					}
				}
				if got := slices.Collect(m.Seams().Trials.Samples("missing")); got != nil {
					t.Fatalf("samples for unknown trial: %+v", got)
				}
			})
		}
	}
}

func TestInMemorySampleRetentionDoesNotGrowWithDuration(t *testing.T) {
	m := newMachine(t, Config{Cores: 2})
	r := running{m: m, spec: machine.TrialSpec{ID: "0001", Cores: []int{0, 1}, Workload: machine.Workload{Threads: 1}}}
	allocations := func(ran time.Duration) float64 {
		return testing.AllocsPerRun(2, func() {
			if err := r.sampleConditions(ran, 1); err != nil {
				t.Fatal(err)
			}
		})
	}
	short := allocations(3 * time.Second)
	long := allocations(time.Hour)
	if long > short+1 {
		t.Fatalf("retained sample allocations grow with duration: short %.0f, long %.0f", short, long)
	}
	var first *machine.TrialConditions
	for sample := range m.Seams().Trials.Samples("0001") {
		first = &sample
		break
	}
	want := &machine.TrialConditions{ElapsedMS: 1000, WorkerCPUMS: map[int]int64{0: 1000, 1: 1000}}
	if diff := cmp.Diff(want, first); diff != "" {
		t.Fatal(diff)
	}
}

func TestSampleSetupFailuresRemainErrors(t *testing.T) {
	for _, name := range []string{"directory", "file"} {
		t.Run(name, func(t *testing.T) {
			m := newMachine(t, Config{Cores: 2, Edges: flat(2, -50, -50)})
			dir := t.TempDir()
			want := "create simulated sample directory"
			if name == "directory" {
				dir = filepath.Join(dir, "not-directory")
				if err := os.WriteFile(dir, nil, 0600); err != nil {
					t.Fatal(err)
				}
			} else {
				want = "create simulated samples"
				if err := os.MkdirAll(filepath.Join(dir, "0001", "samples.jsonl"), 0700); err != nil {
					t.Fatal(err)
				}
			}
			m.SetSamplesDir(dir)
			res, err := runSpec(t, m, "0001", machine.R1, machine.PickWorkload(machine.R1, 0), []int{0}, 3*time.Second, nil)
			if err == nil || !strings.Contains(err.Error(), want) || res.Ran != 3*time.Second {
				t.Fatalf("sample failure result = %+v, %v", res, err)
			}
		})
	}
}
