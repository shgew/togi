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

	"github.com/google/go-cmp/cmp"
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

type recordedSignal struct {
	Core   int
	Signal machine.Signal
	Detail string
}

type recorder struct {
	progress    []string
	samples     []machine.Sample
	signals     []machine.Signal
	diagnostics []recordedSignal
}

func (r *recorder) Progress(s string)       { r.progress = append(r.progress, s) }
func (r *recorder) Sample(s machine.Sample) { r.samples = append(r.samples, s) }
func (r *recorder) Signal(core int, signal machine.Signal, detail string) {
	r.signals = append(r.signals, signal)
	r.diagnostics = append(r.diagnostics, recordedSignal{core, signal, detail})
}

func testSpec(id string, regime machine.Regime, d time.Duration) machine.TrialSpec {
	return machine.TrialSpec{ID: id, Regime: regime, Workload: machine.Workload{Backend: machine.Mprime, DutyPct: 50}, Cores: []int{0}, CPUs: []int{0}, Duration: d}
}

func fakeOptions(t *testing.T, mode string) Options {
	t.Helper()
	dir := t.TempDir()
	if err := os.Chmod(filepath.Dir(dir), 0711); err != nil {
		t.Fatal(err)
	}
	return Options{
		Dir: dir, NoScope: true,
		User:           testIdentity(),
		Backends:       map[machine.Backend]backend.Backend{machine.Mprime: fakeBackend{mode}},
		SampleInterval: 50 * time.Millisecond, StallGrace: time.Hour,
		Hwmon: t.TempDir(), CPUFreq: t.TempDir(), Powercap: t.TempDir(),
		Cores: []machine.CoreInfo{{Core: 0, CCD: 0, CPUs: []int{0}}, {Core: 1, CCD: 1, CPUs: []int{1}}},
	}
}

func testIdentity() Identity {
	if os.Geteuid() == 0 || os.Getegid() == 0 {
		return Identity{UID: 1001, GID: 1001}
	}
	return Identity{UID: uint32(os.Geteuid()), GID: uint32(os.Getegid())}
}

func TestWait(t *testing.T) {
	for _, tt := range []struct {
		name, mode    string
		duration      time.Duration
		regime        machine.Regime
		cores, cpus   []int
		stallGrace    time.Duration
		stallWindow   time.Duration
		cancelAfter   time.Duration
		wantErr       error
		signal        machine.Signal
		wantInstances int
		escaped       []int
		sampleWarning string
		minStops      int
		wantProgress  bool
		wantRan       time.Duration
	}{
		{
			name: "pass", mode: "work", duration: 600 * time.Millisecond,
			regime: machine.R1, cores: []int{0}, cpus: []int{0},
			stallGrace: 200 * time.Millisecond, stallWindow: 300 * time.Millisecond,
			wantProgress: true, wantRan: 600 * time.Millisecond,
		},
		{
			name: "error", mode: "error", duration: time.Second,
			regime: machine.R1, cores: []int{0}, cpus: []int{0},
			stallGrace: time.Hour, signal: machine.ComputationError,
		},
		{
			name: "exit", mode: "exit", duration: time.Second,
			regime: machine.R1, cores: []int{0}, cpus: []int{0},
			stallGrace: time.Hour, signal: machine.UnexpectedExit,
		},
		{
			name: "watched", mode: "watched", duration: time.Second,
			regime: machine.R1, cores: []int{0}, cpus: []int{0},
			stallGrace: time.Hour, signal: machine.ComputationError,
		},
		{
			name: "stall", mode: "sleep", duration: 900 * time.Millisecond,
			regime: machine.R1, cores: []int{0}, cpus: []int{0},
			stallGrace: 200 * time.Millisecond, stallWindow: 300 * time.Millisecond,
			signal: machine.Stall,
		},
		{
			name: "cancel", mode: "work", duration: time.Second,
			regime: machine.R1, cores: []int{0}, cpus: []int{0}, stallGrace: time.Hour,
			cancelAfter: 150 * time.Millisecond, wantErr: context.Canceled,
		},
		{
			name: "R4", mode: "work", duration: 1500 * time.Millisecond,
			regime: machine.R4, cores: []int{0}, cpus: []int{0},
			stallGrace: time.Hour, minStops: 5,
		},
		{
			name: "R7", mode: "work", duration: 800 * time.Millisecond,
			regime: machine.R7, cores: []int{0, 1}, cpus: []int{0, 1},
			stallGrace: time.Hour, wantInstances: 2,
		},
		{
			name: "escape", mode: "escape", duration: time.Second,
			regime: machine.R1, cores: []int{0}, cpus: []int{0}, stallGrace: time.Hour,
			escaped: []int{9}, sampleWarning: "outside allowed cpus",
		},
	} {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				o := fakeOptions(t, tt.mode)
				o.StallGrace = tt.stallGrace
				o.StallWindow = tt.stallWindow
				h := &fakeHost{}
				r := New(o)
				r.host = h
				spec := testSpec(tt.name, tt.regime, tt.duration)
				spec.Cores, spec.CPUs = tt.cores, tt.cpus
				ctx, cancel := context.WithCancel(context.Background())
				defer cancel()
				running, err := r.Start(ctx, spec)
				if err != nil {
					t.Fatal(err)
				}
				if tt.wantInstances != 0 && len(running.Started().Instances) != tt.wantInstances {
					t.Fatal("missing instances")
				}
				if tt.cancelAfter != 0 {
					timer := time.AfterFunc(tt.cancelAfter, cancel)
					defer timer.Stop()
				}
				rec := &recorder{}
				result, err := running.Wait(ctx, rec)
				if tt.wantErr != nil {
					if !errors.Is(err, tt.wantErr) {
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
				if tt.signal != "" && (len(rec.signals) != 1 || rec.signals[0] != tt.signal) {
					t.Fatalf("backend failure signals = %v, want %s", rec.signals, tt.signal)
				}
				if !slices.Equal(result.Escaped, tt.escaped) {
					t.Fatalf("escaped: %+v", result)
				}
				if tt.sampleWarning != "" && (len(rec.samples) == 0 || rec.samples[0].Warning != tt.sampleWarning) {
					t.Fatalf("escape result %+v samples %+v", result, rec.samples)
				}
				if tt.minStops != 0 && (result.Stops < tt.minStops || result.Stops-result.Conts < 0 || result.Stops-result.Conts > 1) {
					t.Fatalf("toggle counts: %+v", result)
				}
				if tt.wantProgress && len(rec.progress) == 0 {
					t.Fatal("no progress captured")
				}
				if tt.wantRan != 0 && result.Ran != tt.wantRan {
					t.Fatalf("healthy trial ended at %s, want %s", result.Ran, tt.wantRan)
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
		trial.drainEvents(context.Background(), &result, &rec, true)
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

func TestPartialStartRollbackBound(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		o := fakeOptions(t, "held-output")
		o.NoScope = false
		var scopes []string
		h := &fakeHost{startFail: 2, killScope: func(scope string) ([]byte, error) {
			scopes = append(scopes, scope)
			return nil, nil
		}}
		r := New(o)
		r.host = h
		spec := testSpec("rollback", machine.R7, time.Second)
		spec.Cores, spec.CPUs = []int{0, 1}, []int{0, 1}
		started := time.Now()
		_, err := r.Start(context.Background(), spec)
		if !errors.Is(err, machine.ErrContainment) || !strings.Contains(err.Error(), "injected start failure") || time.Since(started) > 15*time.Second {
			t.Fatalf("rollback = %v after %s", err, time.Since(started))
		}
		if !slices.Equal(scopes, []string{"togi-trial-rollback-c00", "togi-trial-rollback-c01"}) {
			t.Fatalf("rollback scopes = %v", scopes)
		}
	})
}

type deadlineScopeHost struct {
	*fakeHost
	scopes []string
}

func (h *deadlineScopeHost) KillScope(ctx context.Context, scope string) ([]byte, error) {
	h.scopes = append(h.scopes, scope)
	<-ctx.Done()
	return []byte("Unit not loaded"), ctx.Err()
}

func TestTeardownSharedDeadline(t *testing.T) {
	for _, count := range []int{1, 8, 32} {
		t.Run(strconv.Itoa(count), func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				o := fakeOptions(t, "held-output")
				o.NoScope = false
				h := &deadlineScopeHost{fakeHost: &fakeHost{inScope: true}}
				r := New(o)
				r.host = h
				spec := testSpec("shared", machine.R7, time.Second)
				spec.Cores, spec.CPUs = make([]int, count), make([]int, count)
				for i := range count {
					spec.Cores[i], spec.CPUs[i] = i, i
				}
				trial, err := r.Start(context.Background(), spec)
				if err != nil {
					t.Fatal(err)
				}
				started := time.Now()
				_, err = trial.Wait(context.Background(), &recorder{})
				if !errors.Is(err, machine.ErrContainment) || !errors.Is(err, context.DeadlineExceeded) {
					t.Fatalf("unconfirmed cleanup = %v", err)
				}
				if elapsed := time.Since(started) - spec.Duration; elapsed != 15*time.Second {
					t.Fatalf("%d instances teardown = %s, want 15s", count, elapsed)
				}
				var wantScopes []string
				var wantSignals []fakeSignal
				for i := range count {
					wantScopes = append(wantScopes, fmt.Sprintf("togi-trial-shared-c%02d", i))
					wantSignals = append(wantSignals, fakeSignal{1000 + i, syscall.SIGCONT}, fakeSignal{1000 + i, syscall.SIGTERM})
				}
				for i := range count {
					wantSignals = append(wantSignals, fakeSignal{1000 + i, syscall.SIGKILL})
				}
				if diff := cmp.Diff(wantScopes, h.scopes); diff != "" {
					t.Fatalf("scope kills (-want +got):\n%s", diff)
				}
				if diff := cmp.Diff(wantSignals, h.recordedSignals(), cmp.AllowUnexported(fakeSignal{})); diff != "" {
					t.Fatalf("group signals (-want +got):\n%s", diff)
				}
			})
		})
	}
}

func TestTeardownFinalOutput(t *testing.T) {
	for _, confirmed := range []bool{false, true} {
		t.Run(strconv.FormatBool(confirmed), func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				o := fakeOptions(t, "held-output")
				o.NoScope = false
				h := &fakeHost{inScope: true}
				h.killScope = func(string) ([]byte, error) {
					if confirmed {
						h.procs[0].stdoutW.Close()
						h.procs[0].stderrW.Close()
					}
					return nil, nil
				}
				r := New(o)
				r.host = h
				trial, err := r.Start(context.Background(), testSpec("final-output", machine.R1, time.Second))
				if err != nil {
					t.Fatal(err)
				}
				result, err := trial.Wait(context.Background(), &recorder{})
				if confirmed && err != nil || !confirmed && !errors.Is(err, machine.ErrContainment) {
					t.Fatalf("confirmed %v cleanup = %v", confirmed, err)
				}
				if result.Signal != machine.ComputationError {
					t.Fatalf("final unterminated error was lost: %+v", result)
				}
			})
		})
	}
}

func TestPipeReadFailureIsContainment(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		o := fakeOptions(t, "read-error")
		o.NoScope = false
		r := New(o)
		r.host = &fakeHost{inScope: true}
		trial, err := r.Start(context.Background(), testSpec("read-error", machine.R1, time.Minute))
		if err != nil {
			t.Fatal(err)
		}
		result, err := trial.Wait(context.Background(), &recorder{})
		if !errors.Is(err, syscall.EIO) || !errors.Is(err, machine.ErrContainment) {
			t.Fatalf("unconfirmed pipe drain: result=%+v err=%v", result, err)
		}
		t.Logf("non-EOF pipe read: %v; cleanup is containment failure", err)
	})
}

func TestPartialStartConfirmedRollback(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		o := fakeOptions(t, "work")
		o.NoScope = false
		h := &fakeHost{startFail: 2}
		r := New(o)
		r.host = h
		spec := testSpec("confirmed-rollback", machine.R7, time.Second)
		spec.Cores, spec.CPUs = []int{0, 1}, []int{0, 1}
		_, err := r.Start(context.Background(), spec)
		if err == nil || errors.Is(err, machine.ErrContainment) || !strings.Contains(err.Error(), "injected start failure") {
			t.Fatalf("confirmed rollback = %v", err)
		}
	})
}

func TestStopIsIdempotent(t *testing.T) {
	for _, mode := range []string{"work", "held-output"} {
		t.Run(mode, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				h := &fakeHost{}
				r := New(fakeOptions(t, mode))
				r.host = h
				trial, err := r.Start(context.Background(), testSpec("stop", machine.R1, time.Second))
				if err != nil {
					t.Fatal(err)
				}
				run := trial.(*running)
				err = run.Stop()
				if (mode == "held-output") != errors.Is(err, machine.ErrContainment) {
					t.Fatalf("stop %s = %v", mode, err)
				}
				signals := h.recordedSignals()
				stopped := time.Now()
				again := run.Stop()
				if !errors.Is(again, err) || time.Now() != stopped {
					t.Fatalf("repeated stop retried or lost cleanup failure: %v, %v", err, again)
				}
				if diff := cmp.Diff(signals, h.recordedSignals(), cmp.AllowUnexported(fakeSignal{})); diff != "" {
					t.Fatalf("repeated stop signaled groups again (-want +got):\n%s", diff)
				}
				for _, inst := range run.instances {
					select {
					case <-inst.joined:
					default:
						t.Fatal("stop returned before the process waiter joined")
					}
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
	failed := filepath.Join(dir, "9898")
	if err := os.Mkdir(failed, 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(failed, "stderr"), []byte("failure evidence"), 0600); err != nil {
		t.Fatal(err)
	}
	for i := range 202 {
		id := fmt.Sprintf("%04d", 9899+i)
		if err := os.Mkdir(filepath.Join(dir, id), 0755); err != nil {
			t.Fatal(err)
		}
		if err := r.Passed(id); err != nil {
			t.Fatal(err)
		}
	}
	for _, id := range []string{"9899", "9900"} {
		if _, err := os.Stat(filepath.Join(dir, id)); !os.IsNotExist(err) {
			t.Fatalf("%s retained: %v", id, err)
		}
	}
	for _, id := range []string{"9901", "10100", "9898"} {
		if _, err := os.Stat(filepath.Join(dir, id)); err != nil {
			t.Fatal(err)
		}
	}
	if data, err := os.ReadFile(filepath.Join(failed, "stderr")); err != nil || string(data) != "failure evidence" {
		t.Fatalf("retention lost failed-trial evidence: %q, %v", data, err)
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
	want := []toggle{
		{At: 50 * time.Millisecond, Stop: true, Instances: []int{0}},
		{At: 100 * time.Millisecond, Instances: []int{0}},
		{At: 150 * time.Millisecond, Stop: true, Instances: []int{0}},
		{At: 200 * time.Millisecond, Instances: []int{0}},
		{At: 250 * time.Millisecond, Stop: true, Instances: []int{0}},
	}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("R4 schedule (-want +got):\n%s", diff)
	}
	spec.Regime = machine.R6
	spec.Cores = []int{0, 1, 2, 3}
	spec.Duration = 9 * time.Second
	got = slices.Collect(plan(spec, cores))
	want = []toggle{
		{At: 4500 * time.Millisecond, Instances: []int{0}},
		{At: 4600 * time.Millisecond, Stop: true, Instances: []int{0}},
		{At: 6500 * time.Millisecond, Instances: []int{1}},
		{At: 6600 * time.Millisecond, Stop: true, Instances: []int{1}},
		{At: 8500 * time.Millisecond, Instances: []int{2}},
		{At: 8600 * time.Millisecond, Stop: true, Instances: []int{2}},
	}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("R6 schedule (-want +got):\n%s", diff)
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
	value, _ := readTemperatures(dir)
	if diff := cmp.Diff(new(61), value); diff != "" {
		t.Fatal(diff)
	}
}

func TestR6PlanTermination(t *testing.T) {
	for _, stopAfter := range []int{1, 2} {
		t.Run(string(rune('0'+stopAfter)), func(t *testing.T) {
			spec := testSpec("idle", machine.R6, 5*time.Second)
			var got []toggle
			cores := []machine.CoreInfo{{Core: 0}}
			for step := range plan(spec, cores) {
				got = append(got, step)
				if len(got) == stopAfter {
					break
				}
			}
			want := []toggle{{At: 2500 * time.Millisecond, Instances: []int{0}}, {At: 2600 * time.Millisecond, Stop: true, Instances: []int{0}}}
			want = want[:stopAfter]
			if diff := cmp.Diff(want, got); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

type prepareFailureBackend struct {
	fakeBackend
	prepare func(string) (backend.Launch, error)
}

func (b prepareFailureBackend) Prepare(_ machine.Workload, dir string, _ []int) (backend.Launch, error) {
	return b.prepare(dir)
}

func TestPreparationFailureDoesNotLaunch(t *testing.T) {
	for _, failure := range []string{"backend missing", "prepare", "empty argv", "stdout", "stderr", "trial directory"} {
		t.Run(failure, func(t *testing.T) {
			o := fakeOptions(t, "sleep")
			injected := errors.New("injected prepare failure")
			o.Backends[machine.Mprime] = prepareFailureBackend{prepare: func(dir string) (backend.Launch, error) {
				if failure == "prepare" {
					return backend.Launch{}, injected
				}
				if failure == "empty argv" {
					return backend.Launch{}, nil
				}
				if failure == "stdout" || failure == "stderr" {
					if err := os.WriteFile(filepath.Join(dir, failure+".log"), []byte("existing"), 0644); err != nil {
						t.Fatal(err)
					}
				}
				return backend.Launch{Argv: []string{"fake", "sleep"}}, nil
			}}
			want := map[string]string{"backend missing": "not configured", "prepare": "prepare fake", "empty argv": "empty argv", "stdout": "open stdout log", "stderr": "open stderr log", "trial directory": "trial directory"}[failure]
			if failure == "backend missing" {
				delete(o.Backends, machine.Mprime)
			}
			if failure == "trial directory" {
				path := filepath.Join(o.Dir, "obstacle")
				if err := os.WriteFile(path, nil, 0644); err != nil {
					t.Fatal(err)
				}
				o.Dir = filepath.Join(path, "trials")
			}
			r := New(o)
			h := &fakeHost{}
			r.host = h
			spec := testSpec("prepare", machine.R1, time.Second)
			started, err := r.Start(context.Background(), spec)
			if started != nil || err == nil || !strings.Contains(err.Error(), want) || len(h.procs) != 0 || errors.Is(err, machine.ErrContainment) {
				t.Fatalf("preparation failure started=%v err=%v processes=%d", started, err, len(h.procs))
			}
			if failure == "prepare" && !errors.Is(err, injected) {
				t.Fatalf("prepare cause lost: %v", err)
			}
		})
	}
}

type stopFailureHost struct {
	*fakeHost
	failure error
}

func (h stopFailureHost) SignalGroup(p process, sig syscall.Signal) error {
	if sig == syscall.SIGSTOP {
		return h.failure
	}
	return h.fakeHost.SignalGroup(p, sig)
}

func TestLoadStepFailureContainsWorkload(t *testing.T) {
	for _, regime := range []machine.Regime{machine.R4, machine.R6} {
		for _, cleanupFailure := range []bool{false, true} {
			t.Run(string(regime)+"/"+map[bool]string{false: "clean", true: "failed cleanup"}[cleanupFailure], func(t *testing.T) {
				synctest.Test(t, func(t *testing.T) {
					o := fakeOptions(t, "sleep")
					o.NoScope = false
					failure := errors.New("load step refused")
					cleanup := errors.New("scope kill refused")
					h := stopFailureHost{fakeHost: &fakeHost{inScope: true}, failure: failure}
					if cleanupFailure {
						h.killScope = func(string) ([]byte, error) { return nil, cleanup }
					}
					r := New(o)
					r.host = h
					started, err := r.Start(context.Background(), testSpec("signal", regime, time.Second))
					if regime == machine.R4 {
						if err != nil {
							t.Fatal(err)
						}
						_, err = started.Wait(context.Background(), &recorder{})
					} else if started != nil {
						t.Fatal("initial stop failure returned a running trial")
					}
					if !errors.Is(err, failure) || errors.Is(err, machine.ErrContainment) != cleanupFailure || errors.Is(err, cleanup) != cleanupFailure {
						t.Fatalf("load-step and cleanup failures: %v", err)
					}
					if !slices.Contains(h.recordedSignals(), fakeSignal{1000, syscall.SIGTERM}) {
						t.Fatal("load-step failure did not terminate workload")
					}
					select {
					case <-h.procs[0].exited:
					default:
						t.Fatal("workload survived")
					}
				})
			})
		}
	}
}

func TestSystemdStartupDiagnosticsAreInconclusive(t *testing.T) {
	for _, line := range []string{"Failed to start transient scope unit: denied", "Failed to execute backend: missing"} {
		t.Run(line, func(t *testing.T) {
			trial := &running{backend: fakeBackend{}}
			inst := &instance{Core: 7, unexpected: true}
			var result machine.Result
			var report recorder
			if !trial.classify(inst, line, true, &result, &report) || trial.classifyExit(inst, &result, &report) || result.Signal != "" || !strings.Contains(result.Inconclusive, line) || len(report.signals) != 0 {
				t.Fatalf("setup failure became stability evidence: %+v", result)
			}
		})
	}
}

type scopeStopHost struct {
	*fakeHost
	began     time.Time
	stoppedAt []time.Duration
	wasReady  []bool
}

func (h *scopeStopHost) SignalGroup(p process, sig syscall.Signal) error {
	if sig == syscall.SIGSTOP {
		h.mu.Lock()
		h.stoppedAt = append(h.stoppedAt, time.Since(h.began))
		h.wasReady = append(h.wasReady, h.inScope)
		h.mu.Unlock()
	}
	return h.fakeHost.SignalGroup(p, sig)
}

func TestR6ScopeReadinessBeforeInitialStop(t *testing.T) {
	for _, ending := range []string{"ready", "timeout", "cancel"} {
		t.Run(ending, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				o := fakeOptions(t, "sleep")
				o.NoScope = false
				o.StallGrace = 100 * time.Millisecond
				h := &scopeStopHost{fakeHost: &fakeHost{}}
				r := New(o)
				r.host = h
				ctx, cancel := context.WithCancel(context.Background())
				defer cancel()
				if ending != "timeout" {
					time.AfterFunc(45*time.Millisecond, func() {
						if ending == "cancel" {
							cancel()
						} else {
							h.mu.Lock()
							h.inScope = true
							h.mu.Unlock()
						}
					})
				}
				begin := time.Now()
				h.began = begin
				trial, err := r.Start(ctx, testSpec("readiness", machine.R6, time.Second))
				if err != nil {
					t.Fatal(err)
				}
				run := trial.(*running)
				if run.instances[0].ready != (ending == "ready") {
					t.Fatalf("scope readiness = %t for %s", run.instances[0].ready, ending)
				}
				wantElapsed := 50 * time.Millisecond
				if ending == "timeout" {
					wantElapsed = 100 * time.Millisecond
				}
				if ending == "cancel" {
					wantElapsed = 45 * time.Millisecond
				}
				if time.Since(begin) != wantElapsed {
					t.Fatalf("readiness wait = %s, want %s", time.Since(begin), wantElapsed)
				}
				if diff := cmp.Diff([]fakeSignal{{1000, syscall.SIGSTOP}}, h.recordedSignals(), cmp.AllowUnexported(fakeSignal{})); diff != "" {
					t.Fatal(diff)
				}
				if diff := cmp.Diff([]time.Duration{wantElapsed}, h.stoppedAt); diff != "" {
					t.Fatalf("initial stop preceded readiness boundary (-want +got):\n%s", diff)
				}
				if diff := cmp.Diff([]bool{ending == "ready"}, h.wasReady); diff != "" {
					t.Fatalf("readiness at initial stop (-want +got):\n%s", diff)
				}
				if err := run.Stop(); err != nil {
					t.Fatal(err)
				}
				select {
				case <-run.instances[0].joined:
				default:
					t.Fatal("initially stopped workload survived cleanup")
				}
			})
		})
	}
}

func TestPassedMissingTrialDirectory(t *testing.T) {
	root := t.TempDir()
	r := New(Options{Dir: root})
	if err := r.Passed("0001"); !errors.Is(err, os.ErrNotExist) || !strings.Contains(err.Error(), "mark trial 0001 passed") {
		t.Fatalf("missing marker directory: %v", err)
	}
}
