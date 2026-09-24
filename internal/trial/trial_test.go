package trial

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"
	"unsafe"

	"code.marleb.org/shgew/shycler/internal/backend"
	"code.marleb.org/shgew/shycler/internal/machine"
)

type helperBackend struct{ mode string }

func (h helperBackend) Name() string           { return "helper" }
func (h helperBackend) Check() (string, error) { return "ok", nil }
func (h helperBackend) Prepare(_ machine.Workload, _ string, cpus []int) (backend.Launch, error) {
	launch := backend.Launch{Argv: []string{"taskset", "-c", strconv.Itoa(cpus[0]), os.Args[0], "-test.run=TestHelperProcess", "--", "--helper", h.mode}}
	if strings.HasPrefix(h.mode, "watched") {
		launch.Watch = []string{"results.txt"}
	}
	return launch, nil
}
func (h helperBackend) Classify(line string) backend.Line {
	if strings.Contains(line, "COMPUTE ERROR") {
		return backend.Line{Kind: backend.ComputationError}
	}
	if strings.HasPrefix(line, "PIN FAILED") {
		return backend.Line{Kind: backend.SetupError}
	}
	if strings.HasPrefix(line, "AFFINITY:") {
		cpu, _ := strconv.Atoi(strings.TrimPrefix(line, "AFFINITY:"))
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
}

func (r *recorder) Progress(s string)       { r.progress = append(r.progress, s) }
func (r *recorder) Sample(s machine.Sample) { r.samples = append(r.samples, s) }

func TestHelperProcess(t *testing.T) {
	idx := slices.Index(os.Args, "--helper")
	if idx < 0 {
		return
	}
	mode := os.Args[idx+1]
	if strings.HasPrefix(mode, "escape:") {
		runtime.LockOSThread()
		target, err := strconv.Atoi(strings.TrimPrefix(mode, "escape:"))
		if err != nil {
			os.Exit(2)
		}
		mask := make([]byte, target/8+1)
		mask[target/8] = 1 << uint(target%8)
		_, _, errno := syscall.RawSyscall(syscall.SYS_SCHED_SETAFFINITY, 0, uintptr(len(mask)), uintptr(unsafe.Pointer(&mask[0])))
		if errno != 0 {
			fmt.Println("PIN FAILED:", errno)
			os.Exit(0)
		}
		until := time.Now().Add(5 * time.Second)
		for time.Now().Before(until) {
		}
		os.Exit(0)
	}
	switch mode {
	case "exit":
		os.Exit(0)
	case "error":
		time.Sleep(300 * time.Millisecond)
		fmt.Println("COMPUTE ERROR")
		os.Exit(0)
	case "watched":
		if err := os.WriteFile("results.txt", []byte("COMPUTE ERROR\n"), 0644); err != nil {
			os.Exit(2)
		}
		os.Exit(0)
	case "watched-precedence":
		if err := os.WriteFile("results.txt", []byte("PIN FAILED\nCOMPUTE ERROR\nAFFINITY:42\n"), 0644); err != nil {
			os.Exit(2)
		}
		os.Exit(0)
	case "setup-then-error":
		fmt.Println("PIN FAILED")
		fmt.Println("COMPUTE ERROR")
		os.Exit(0)
	case "error-then-affinity":
		fmt.Println("COMPUTE ERROR")
		fmt.Println("AFFINITY:42")
		os.Exit(0)
	case "descendant":
		cmd := exec.Command(os.Args[0], "-test.run=TestHelperProcess", "--", "--helper", "orphan")
		cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true}
		if err := cmd.Start(); err != nil {
			os.Exit(2)
		}
		if err := os.WriteFile("descendant.pid", []byte(strconv.Itoa(cmd.Process.Pid)), 0644); err != nil {
			os.Exit(2)
		}
		os.Exit(0)
	case "orphan":
		for {
			_, _, _ = syscall.RawSyscall(syscall.SYS_PAUSE, 0, 0, 0)
		}
	case "sleep":
		time.Sleep(5 * time.Second)
		os.Exit(0)
	case "work":
		deadline := time.Now().Add(5 * time.Second)
		for time.Now().Before(deadline) {
			until := time.Now().Add(100 * time.Millisecond)
			for time.Now().Before(until) {
			}
			fmt.Println("progress 1")
		}
		os.Exit(0)
	}
	os.Exit(2)
}

func testCPUs(t *testing.T) []int {
	t.Helper()
	b, err := os.ReadFile("/proc/self/status")
	if err != nil {
		t.Fatal(err)
	}
	for line := range strings.SplitSeq(string(b), "\n") {
		if strings.HasPrefix(line, "Cpus_allowed_list:") {
			ranges := strings.Split(strings.TrimSpace(strings.TrimPrefix(line, "Cpus_allowed_list:")), ",")
			var cpus []int
			for _, part := range ranges {
				bounds := strings.SplitN(part, "-", 2)
				start, err := strconv.Atoi(bounds[0])
				if err != nil {
					t.Fatal(err)
				}
				end := start
				if len(bounds) == 2 {
					end, err = strconv.Atoi(bounds[1])
					if err != nil {
						t.Fatal(err)
					}
				}
				for cpu := start; cpu <= end && len(cpus) < 2; cpu++ {
					cpus = append(cpus, cpu)
				}
				if len(cpus) == 2 {
					break
				}
			}
			if len(cpus) < 2 {
				t.Skip("two CPUs needed")
			}
			return cpus
		}
	}
	t.Fatal("missing CPU affinity list")
	return nil
}
func testOptions(t *testing.T, mode string) Options {
	t.Helper()
	cpus := testCPUs(t)
	return Options{Dir: t.TempDir(), NoScope: true, Backends: map[machine.Backend]backend.Backend{machine.Mprime: helperBackend{mode}}, SampleInterval: 50 * time.Millisecond, StallGrace: time.Hour, Cores: []machine.CoreInfo{{Core: 0, CCD: 0, CPUs: []int{cpus[0]}}, {Core: 1, CCD: 1, CPUs: []int{cpus[1]}}}}
}
func testSpec(id string, regime machine.Regime, d time.Duration) machine.TrialSpec {
	return machine.TrialSpec{ID: id, Regime: regime, Workload: machine.Workload{Backend: machine.Mprime, DutyPct: 50}, Cores: []int{0}, CPUs: []int{0}, Duration: d}
}
func TestTrials(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name, mode string
		duration   time.Duration
		signal     machine.Signal
		cancel     bool
	}{
		{"pass", "work", 600 * time.Millisecond, "", false},
		{"error", "error", time.Second, machine.ComputationError, false},
		{"exit", "exit", time.Second, machine.UnexpectedExit, false},
		{"watched", "watched", time.Second, machine.ComputationError, false},
		{"stall", "sleep", 900 * time.Millisecond, machine.Stall, false},
		{"cancel", "work", time.Second, "", true},
		{"R4", "work", 1500 * time.Millisecond, "", false},
	}
	for _, tt := range cases {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			o := testOptions(t, tt.mode)
			if tt.name == "stall" {
				o.StallGrace = 200 * time.Millisecond
				o.StallWindow = 300 * time.Millisecond
			}
			r := New(o)
			spec := testSpec(tt.name, machine.R1, tt.duration)
			spec.CPUs = []int{o.Cores[0].CPUs[0]}
			if tt.name == "R4" {
				spec.Regime = machine.R4
			}
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			running, err := r.Start(ctx, spec)
			if err != nil {
				t.Fatal(err)
			}
			if tt.cancel {
				time.AfterFunc(150*time.Millisecond, cancel)
			}
			rec := &recorder{}
			result, err := running.Wait(ctx, rec)
			if tt.cancel {
				if err != context.Canceled {
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
	}
	t.Run("R7", func(t *testing.T) {
		t.Parallel()
		o := testOptions(t, "work")
		r := New(o)
		spec := testSpec("R7", machine.R7, 800*time.Millisecond)
		spec.Cores = []int{0, 1}
		spec.CPUs = []int{o.Cores[0].CPUs[0], o.Cores[1].CPUs[0]}
		running, err := r.Start(context.Background(), spec)
		if err != nil {
			t.Fatal(err)
		}
		if len(running.Started().Instances) != 2 {
			t.Fatal("missing instances")
		}
		rec := &recorder{}
		result, err := running.Wait(context.Background(), rec)
		if err != nil || result.Signal != "" || len(result.Escaped) > 0 {
			t.Fatalf("result %+v err %v", result, err)
		}
		joined := strings.Join(rec.progress, "\n")
		if !strings.Contains(joined, "CCD0 only") || !strings.Contains(joined, "CCD1 only") {
			t.Fatalf("phase progress: %s", joined)
		}
	})
	t.Run("escape", func(t *testing.T) {
		t.Parallel()
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
	})
}

func TestScopeNeverReady(t *testing.T) {
	o := testOptions(t, "work")
	spec := testSpec("not-ready", machine.R1, 200*time.Millisecond)
	spec.CPUs = []int{o.Cores[0].CPUs[0]}
	r, err := New(o).Start(context.Background(), spec)
	if err != nil {
		t.Fatal(err)
	}
	trial := r.(*running)
	trial.options.NoScope = false
	bin := t.TempDir()
	if err := os.WriteFile(filepath.Join(bin, "systemctl"), []byte("#!/bin/sh\necho 'Unit not loaded' >&2\nexit 1\n"), 0755); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", bin+":"+os.Getenv("PATH"))
	var rec recorder
	result, err := trial.Wait(context.Background(), &rec)
	if err != nil || result.Signal != "" || !strings.Contains(result.Inconclusive, "never entered scope") {
		t.Fatalf("unready scope result %+v, err %v", result, err)
	}
}

func TestQueuedExitBeforeTeardown(t *testing.T) {
	o := testOptions(t, "exit")
	spec := testSpec("queued-exit", machine.R1, time.Second)
	spec.CPUs = []int{o.Cores[0].CPUs[0]}
	r, err := New(o).Start(context.Background(), spec)
	if err != nil {
		t.Fatal(err)
	}
	trial := r.(*running)
	var exit streamEvent
	timeout := time.After(time.Second)
	for !exit.exit {
		select {
		case exit = <-trial.events:
		case <-timeout:
			t.Fatal("helper did not exit")
		}
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
			o := testOptions(t, tt.mode)
			spec := testSpec(tt.mode, machine.R1, time.Second)
			spec.CPUs = []int{o.Cores[0].CPUs[0]}
			r, err := New(o).Start(context.Background(), spec)
			if err != nil {
				t.Fatal(err)
			}
			var rec recorder
			result, err := r.Wait(context.Background(), &rec)
			if err != nil || result.Signal != tt.signal || !slices.Equal(result.Escaped, tt.escaped) {
				t.Fatalf("conflicting evidence result %+v, err %v", result, err)
			}
		})
	}
}

func TestR6StartsIdle(t *testing.T) {
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

func TestRetention(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	r := New(Options{Dir: dir})
	for i := 0; i < 202; i++ {
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
	if len(got) != 3 || !slices.Equal(got[0].Instances, []int{1, 3}) || !slices.Equal(got[2].Instances, []int{0, 2}) {
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
