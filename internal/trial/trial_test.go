package trial

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"testing/synctest"
	"time"

	"github.com/shgew/togi/internal/backend"
	"github.com/shgew/togi/internal/machine"
)

type fakeBackend struct{ mode string }

func (b fakeBackend) Name() string           { return "fake" }
func (b fakeBackend) Check() (string, error) { return "ok", nil }
func (b fakeBackend) Prepare(_ machine.Workload, _ string, _ []int) (backend.Launch, error) {
	launch := backend.Launch{Argv: []string{"fake", b.mode}}
	if strings.HasPrefix(b.mode, "watched") {
		launch.Watch = []string{"results.txt"}
	}
	return launch, nil
}
func (b fakeBackend) Classify(line string) backend.Line { return classifyHelper(line) }

func classifyHelper(line string) backend.Line {
	if strings.Contains(line, "COMPUTE ERROR") {
		return backend.Line{Kind: backend.ComputationError}
	}
	if strings.HasPrefix(line, "PIN FAILED") {
		return backend.Line{Kind: backend.SetupError}
	}
	if text, ok := strings.CutPrefix(line, "AFFINITY:"); ok {
		cpu, _ := strconv.Atoi(text)
		return backend.Line{Kind: backend.AffinityError, CPU: cpu}
	}
	if strings.HasPrefix(line, "progress") {
		return backend.Line{Kind: backend.Progress, Detail: line}
	}
	return backend.Line{}
}

type recorder struct {
	progress []string
	samples  []machine.Sample
	signals  []machine.Signal
}

func (r *recorder) Progress(s string)       { r.progress = append(r.progress, s) }
func (r *recorder) Sample(s machine.Sample) { r.samples = append(r.samples, s) }
func (r *recorder) Signal(core int, signal machine.Signal, detail string) {
	r.signals = append(r.signals, signal)
}

func testSpec(id string, regime machine.Regime, d time.Duration) machine.TrialSpec {
	return machine.TrialSpec{ID: id, Regime: regime, Workload: machine.Workload{Backend: machine.Mprime, DutyPct: 50}, Cores: []int{0}, CPUs: []int{0}, Duration: d}
}

func fakeOptions(t *testing.T, mode string) Options {
	t.Helper()
	return Options{
		Dir: t.TempDir(), NoScope: true,
		Backends:       map[machine.Backend]backend.Backend{machine.Mprime: fakeBackend{mode}},
		SampleInterval: 50 * time.Millisecond, StallGrace: time.Hour,
		Hwmon: t.TempDir(),
		Cores: []machine.CoreInfo{{Core: 0, CCD: 0, CPUs: []int{0}}, {Core: 1, CCD: 1, CPUs: []int{1}}},
	}
}

func TestWait(t *testing.T) {
	for _, tt := range []struct {
		name, mode string
		duration   time.Duration
		signal     machine.Signal
	}{
		{"pass", "work", 600 * time.Millisecond, ""},
		{"error", "error", time.Second, machine.ComputationError},
		{"exit", "exit", time.Second, machine.UnexpectedExit},
		{"watched", "watched", time.Second, machine.ComputationError},
		{"stall", "sleep", 900 * time.Millisecond, machine.Stall},
		{"cancel", "work", time.Second, ""},
		{"R4", "work", 1500 * time.Millisecond, ""},
		{"R7", "work", 800 * time.Millisecond, ""},
		{"escape", "escape", time.Second, ""},
	} {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				o := fakeOptions(t, tt.mode)
				if tt.name == "stall" {
					o.StallGrace = 200 * time.Millisecond
					o.StallWindow = 300 * time.Millisecond
				}
				h := &fakeHost{}
				r := New(o)
				r.host = h
				spec := testSpec(tt.name, machine.R1, tt.duration)
				if tt.name == "R4" {
					spec.Regime = machine.R4
				}
				if tt.name == "R7" {
					spec.Regime = machine.R7
					spec.Cores = []int{0, 1}
					spec.CPUs = []int{0, 1}
				}
				ctx, cancel := context.WithCancel(context.Background())
				defer cancel()
				running, err := r.Start(ctx, spec)
				if err != nil {
					t.Fatal(err)
				}
				if tt.name == "R7" && len(running.Started().Instances) != 2 {
					t.Fatal("missing instances")
				}
				if tt.name == "cancel" {
					timer := time.AfterFunc(150*time.Millisecond, cancel)
					defer timer.Stop()
				}
				rec := &recorder{}
				result, err := running.Wait(ctx, rec)
				if tt.name == "cancel" {
					if !errors.Is(err, context.Canceled) {
						t.Fatalf("cancel returned %v", err)
					}
					return
				}
				if err != nil {
					t.Fatal(err)
				}
				if result.Signal != tt.signal {
					t.Fatalf("signal %s, want %s (result %+v)", result.Signal, tt.signal, result)
				}
				if tt.signal == machine.ComputationError && (len(rec.signals) != 1 || rec.signals[0] != machine.ComputationError) {
					t.Fatalf("backend computation signals = %v", rec.signals)
				}
				if tt.name == "escape" {
					if !slices.Equal(result.Escaped, []int{9}) || len(rec.samples) == 0 || rec.samples[0].Warning != "outside allowed cpus" {
						t.Fatalf("escape result %+v samples %+v", result, rec.samples)
					}
					return
				}
				if len(result.Escaped) != 0 {
					t.Fatalf("escaped: %+v", result)
				}
				if tt.name == "R4" && (result.Stops < 5 || result.Stops-result.Conts < 0 || result.Stops-result.Conts > 1) {
					t.Fatalf("toggle counts: %+v", result)
				}
				if tt.name == "pass" && len(rec.progress) == 0 {
					t.Fatal("no progress captured")
				}
			})
		})
	}
}

func TestScopeNeverReady(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		o := fakeOptions(t, "work")
		o.NoScope = false
		h := &fakeHost{killScope: func(string) ([]byte, error) { return []byte("Unit not loaded"), errors.New("exit status 1") }}
		r := New(o)
		r.host = h
		trial, err := r.Start(context.Background(), testSpec("not-ready", machine.R1, 200*time.Millisecond))
		if err != nil {
			t.Fatal(err)
		}
		var rec recorder
		result, err := trial.Wait(context.Background(), &rec)
		if err != nil || result.Signal != "" || !strings.Contains(result.Inconclusive, "never entered scope") {
			t.Fatalf("unready scope result %+v, err %v", result, err)
		}
	})
}

func TestQueuedExitBeforeTeardown(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		o := fakeOptions(t, "exit")
		h := &fakeHost{}
		r := New(o)
		r.host = h
		started, err := r.Start(context.Background(), testSpec("queued-exit", machine.R1, time.Second))
		if err != nil {
			t.Fatal(err)
		}
		trial := started.(*running)
		var exit streamEvent
		for !exit.exit {
			exit = <-trial.events
		}
		trial.events <- exit
		var rec recorder
		var result machine.Result
		trial.drainEvents(&result, &rec, true)
		if err := trial.teardown(&result, &rec); err != nil {
			t.Fatal(err)
		}
		if result.Signal != machine.UnexpectedExit {
			t.Fatalf("queued exit result %+v", result)
		}
	})
}

func TestTeardownEvidencePrecedence(t *testing.T) {
	for _, tt := range []struct {
		mode    string
		signal  machine.Signal
		escaped []int
	}{
		{"setup-then-error", machine.ComputationError, nil},
		{"error-then-affinity", "", []int{42}},
		{"watched-precedence", "", []int{42}},
	} {
		t.Run(tt.mode, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				o := fakeOptions(t, tt.mode)
				h := &fakeHost{}
				r := New(o)
				r.host = h
				running, err := r.Start(context.Background(), testSpec(tt.mode, machine.R1, time.Second))
				if err != nil {
					t.Fatal(err)
				}
				var rec recorder
				result, err := running.Wait(context.Background(), &rec)
				if err != nil || result.Signal != tt.signal || !slices.Equal(result.Escaped, tt.escaped) {
					t.Fatalf("conflicting evidence result %+v, err %v", result, err)
				}
			})
		})
	}
}

func TestR6Bursts(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		o := fakeOptions(t, "work")
		o.SampleInterval = 450 * time.Millisecond
		h := &fakeHost{}
		r := New(o)
		r.host = h
		spec := testSpec("r6-idle", machine.R6, 600*time.Millisecond)
		spec.Cores = []int{0, 1}
		spec.CPUs = []int{0, 1}
		running, err := r.Start(context.Background(), spec)
		if err != nil {
			t.Fatal(err)
		}
		signals := h.recordedSignals()
		if len(signals) != 2 || signals[0] != (fakeSignal{1000, syscall.SIGSTOP}) || signals[1] != (fakeSignal{1001, syscall.SIGSTOP}) {
			t.Fatalf("initial signals: %+v", signals)
		}
		var rec recorder
		result, err := running.Wait(context.Background(), &rec)
		if err != nil || result.Signal != "" || result.Stops != 3 || result.Conts != 1 {
			t.Fatalf("R6 initial stops and bursts result %+v, err %v", result, err)
		}
		idle := slices.IndexFunc(rec.progress, func(line string) bool { return strings.HasPrefix(line, "first half idle") })
		if idle < 0 || slices.ContainsFunc(rec.progress[:idle], func(line string) bool { return strings.HasSuffix(line, "progress 1") }) {
			t.Fatalf("stopped instances made progress before the bursts: %q", rec.progress)
		}
	})
}

func TestRetention(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	r := New(Options{Dir: dir})
	for i := range 202 {
		id := fmt.Sprintf("%04d", i)
		if err := os.Mkdir(filepath.Join(dir, id), 0755); err != nil {
			t.Fatal(err)
		}
		if err := r.Passed(id); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.Mkdir(filepath.Join(dir, "failed"), 0755); err != nil {
		t.Fatal(err)
	}
	for _, id := range []string{"0000", "0001"} {
		if _, err := os.Stat(filepath.Join(dir, id)); !os.IsNotExist(err) {
			t.Fatalf("%s retained: %v", id, err)
		}
	}
	for _, id := range []string{"0002", "0201", "failed"} {
		if _, err := os.Stat(filepath.Join(dir, id)); err != nil {
			t.Fatal(err)
		}
	}
}
func TestPlans(t *testing.T) {
	t.Parallel()
	cores := []machine.CoreInfo{{Core: 0, CCD: 0}, {Core: 1, CCD: 1}, {Core: 2, CCD: 0}, {Core: 3, CCD: 1}}
	spec := testSpec("r3", machine.R3, 6*time.Second)
	spec.Seed = 19
	var got []toggle
	for p := range plan(spec, cores) {
		got = append(got, p)
		if len(got) == 4 {
			break
		}
	}
	var at time.Duration
	index := 0
	for p := range machine.ScheduleFor(spec).Periods() {
		at += p
		if index >= len(got) || at >= spec.Duration {
			break
		}
		if got[index].At != at || got[index].Stop != (index%2 == 0) {
			t.Fatalf("R3 %v", got)
		}
		index++
	}
	spec.Regime = machine.R4
	spec.Duration = 300 * time.Millisecond
	got = slices.Collect(plan(spec, cores))
	if len(got) != 5 || got[0].At != 50*time.Millisecond || got[1].Stop {
		t.Fatalf("R4 %v", got)
	}
	spec.Regime = machine.R6
	spec.Cores = []int{0, 1, 2, 3}
	spec.Duration = 9 * time.Second
	got = slices.Collect(plan(spec, cores))
	if len(got) != 6 || got[0].At != 4500*time.Millisecond || got[0].Instances[0] != 0 || got[2].Instances[0] != 1 {
		t.Fatalf("R6 %v", got)
	}
	spec.Regime = machine.R7
	got = slices.Collect(plan(spec, cores))
	if len(got) != 0 {
		t.Fatalf("R7 %v", got)
	}
}
func TestStalled(t *testing.T) {
	t.Parallel()
	a := cpuSample{active: time.Second, cpu: time.Second}
	if !stalled(a, cpuSample{active: 3 * time.Second, cpu: time.Second + 900*time.Millisecond}, 1) || stalled(a, cpuSample{active: 3 * time.Second, cpu: 2 * time.Second}, 1) {
		t.Fatal("stall threshold")
	}
}
func TestTctl(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	hw := filepath.Join(dir, "hwmon2")
	if err := os.Mkdir(hw, 0755); err != nil {
		t.Fatal(err)
	}
	for n, v := range map[string]string{"name": "zenpower", "temp1_label": "Tdie", "temp1_input": "56000", "temp2_label": "Tctl", "temp2_input": "61000"} {
		if err := os.WriteFile(filepath.Join(hw, n), []byte(v), 0644); err != nil {
			t.Fatal(err)
		}
	}
	if value := readTctl(dir); value == nil || *value != 61 {
		t.Fatalf("Tctl %v", value)
	}
}
