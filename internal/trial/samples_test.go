package trial

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"syscall"
	"testing"
	"testing/synctest"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func sensorFile(t *testing.T, root, path, value string) {
	t.Helper()
	path = filepath.Join(root, path)
	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(value), 0644); err != nil {
		t.Fatal(err)
	}
}

func TestTrialConditionsSamples(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		o := fakeOptions(t, "work")
		o.SampleInterval = time.Second
		o.CPUFreq, o.Powercap = t.TempDir(), t.TempDir()
		o.Cores[0].CPUs = []int{0, 16}
		o.Cores = append(o.Cores, machine.CoreInfo{Core: 2, CPUs: []int{2}})
		for path, value := range map[string]string{
			"hwmon0/name": "k10temp", "hwmon0/temp1_label": "Tctl", "hwmon0/temp1_input": "71000",
			"hwmon0/temp2_label": "Tccd1", "hwmon0/temp2_input": "65000",
			"hwmon0/temp3_label": "Tccd2", "hwmon0/temp3_input": "not a temperature",
			"hwmon0/temp4_label": "Tdie", "hwmon0/temp4_input": "69000",
		} {
			sensorFile(t, o.Hwmon, path, value)
		}
		for path, value := range map[string]string{
			"cpu0/cpufreq/scaling_cur_freq": "5420000", "cpu1/cpufreq/scaling_cur_freq": "5610000",
			"cpu16/cpufreq/scaling_cur_freq": "1000000", "cpu2/cpufreq/scaling_cur_freq": "2000000",
		} {
			sensorFile(t, o.CPUFreq, path, value)
		}
		sensorFile(t, o.Powercap, "intel-rapl:0/energy_uj", "1000000")
		r := New(o)
		r.host = &fakeHost{}
		spec := testSpec("samples", machine.R7, 2500*time.Millisecond)
		spec.Cores, spec.CPUs = []int{0, 1}, []int{0, 1}
		run, err := r.Start(context.Background(), spec)
		if err != nil {
			t.Fatal(err)
		}
		first := time.AfterFunc(500*time.Millisecond, func() { sensorFile(t, o.Powercap, "intel-rapl:0/energy_uj", "11000000") })
		defer first.Stop()
		second := time.AfterFunc(1500*time.Millisecond, func() {
			sensorFile(t, o.Powercap, "intel-rapl:0/energy_uj", "21000000")
			sensorFile(t, o.Hwmon, "hwmon0/temp1_input", "72000")
		})
		defer second.Stop()
		result, err := run.Wait(context.Background(), &recorder{})
		if err != nil {
			t.Fatal(err)
		}
		b, err := os.ReadFile(filepath.Join(o.Dir, spec.ID, "samples.jsonl"))
		if err != nil {
			t.Fatal(err)
		}
		var got []machine.TrialConditions
		for line := range strings.SplitSeq(strings.TrimSuffix(string(b), "\n"), "\n") {
			var p machine.TrialConditions
			if err := json.Unmarshal([]byte(line), &p); err != nil {
				t.Fatal(err)
			}
			got = append(got, p)
		}
		want := []machine.TrialConditions{
			{ElapsedMS: 1000, TctlC: new(71), TccdC: map[string]int{"Tccd1": 65}, CoreMHz: map[int]int{0: 5420, 1: 5610}, PackagePowerW: new(10.0)},
			{ElapsedMS: 2000, TctlC: new(72), TccdC: map[string]int{"Tccd1": 65}, CoreMHz: map[int]int{0: 5420, 1: 5610}, PackagePowerW: new(10.0)},
		}
		if diff := cmp.Diff(want, got); diff != "" {
			t.Fatal(diff)
		}
		if diff := cmp.Diff(new(72), result.TctlMaxC); diff != "" {
			t.Fatal(diff)
		}
		if diff := cmp.Diff(&want[1], r.LastSample(spec.ID)); diff != "" {
			t.Fatal(diff)
		}
	})
}

func TestConditionsMissingAndPowerWrap(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		o := Options{Hwmon: t.TempDir(), CPUFreq: t.TempDir(), Powercap: t.TempDir()}
		started := time.Now()
		s := newConditionsSampler(o, machine.TrialSpec{}, started)
		time.Sleep(time.Second)
		p := s.sample(started)
		b, err := json.Marshal(p)
		if err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff(`{"elapsed_ms":1000}`, string(b)); diff != "" {
			t.Fatal(diff)
		}
		sensorFile(t, o.Powercap, "intel-rapl:0/energy_uj", "9000000")
		sensorFile(t, o.Powercap, "intel-rapl:0/max_energy_range_uj", "10000000")
		s = newConditionsSampler(o, machine.TrialSpec{}, time.Now())
		time.Sleep(time.Second)
		sensorFile(t, o.Powercap, "intel-rapl:0/energy_uj", "1000000")
		if diff := cmp.Diff(new(2.0), s.sample(started).PackagePowerW); diff != "" {
			t.Fatal(diff)
		}
		if err := os.Remove(filepath.Join(o.Powercap, "intel-rapl:0/energy_uj")); err != nil {
			t.Fatal(err)
		}
		time.Sleep(time.Second)
		if diff := cmp.Diff((*float64)(nil), s.sample(started).PackagePowerW); diff != "" {
			t.Fatal(diff)
		}
		sensorFile(t, o.Powercap, "intel-rapl:0/energy_uj", "3000000")
		time.Sleep(time.Second)
		if diff := cmp.Diff((*float64)(nil), s.sample(started).PackagePowerW); diff != "" {
			t.Fatal(diff)
		}
	})
}

type fakeSampleFile struct {
	mu       sync.Mutex
	samples  []machine.TrialConditions
	syncs    int
	writeErr error
	syncFn   func() error
	closeErr error
	closed   bool
}

func (f *fakeSampleFile) Write(b []byte) (int, error) {
	if f.writeErr != nil {
		return 0, f.writeErr
	}
	var sample machine.TrialConditions
	if err := json.Unmarshal(b, &sample); err != nil {
		return 0, err
	}
	f.mu.Lock()
	f.samples = append(f.samples, sample)
	f.mu.Unlock()
	return len(b), nil
}

func (f *fakeSampleFile) Sync() error {
	f.mu.Lock()
	f.syncs++
	f.mu.Unlock()
	if f.syncFn != nil {
		return f.syncFn()
	}
	return nil
}

func (f *fakeSampleFile) Close() error {
	f.closed = true
	return f.closeErr
}

func TestSamplesOpenFailureDuration(t *testing.T) {
	for _, delay := range []time.Duration{2 * time.Second, 5 * time.Second} {
		t.Run(delay.String(), func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				r := New(fakeOptions(t, "work"))
				h := &fakeHost{}
				r.host = h
				run, err := r.Start(context.Background(), testSpec("open-failure", machine.R1, 3*time.Second))
				if err != nil {
					t.Fatal(err)
				}
				openErr := errors.New("injected open failure")
				run.(*running).openSamples = func(string) (sampleFile, error) {
					time.Sleep(delay)
					return nil, openErr
				}
				result, err := run.Wait(context.Background(), &recorder{})
				if !errors.Is(err, openErr) {
					t.Fatalf("open failure lost: %v", err)
				}
				if diff := cmp.Diff(min(delay, 3*time.Second), result.Ran); diff != "" {
					t.Fatal(diff)
				}
				if !slices.Contains(h.recordedSignals(), fakeSignal{1000, syscall.SIGTERM}) {
					t.Fatal("backend was not terminated")
				}
			})
		})
	}
}

func TestSamplesPersistenceFailureEndsTrial(t *testing.T) {
	for _, operation := range []string{"write", "sync"} {
		t.Run(operation, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				o := fakeOptions(t, "work")
				o.SampleInterval = 100 * time.Millisecond
				r := New(o)
				r.host = &fakeHost{}
				run, err := r.Start(context.Background(), testSpec("persist-failure", machine.R1, time.Second))
				if err != nil {
					t.Fatal(err)
				}
				persistErr, closeErr := errors.New("injected persistence failure"), errors.New("injected close failure")
				f := &fakeSampleFile{closeErr: closeErr}
				if operation == "write" {
					f.writeErr = persistErr
				} else {
					f.syncFn = func() error { return persistErr }
				}
				run.(*running).openSamples = func(string) (sampleFile, error) { return f, nil }
				result, err := run.Wait(context.Background(), &recorder{})
				if !errors.Is(err, persistErr) || !errors.Is(err, closeErr) {
					t.Fatalf("persistence or close failure lost: %v", err)
				}
				if diff := cmp.Diff(100*time.Millisecond, result.Ran); diff != "" {
					t.Fatal(diff)
				}
				if !f.closed {
					t.Fatal("samples file remained open")
				}
			})
		})
	}
}

func TestSlowSamplesPreserveScheduleAndJoinWriter(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		o := fakeOptions(t, "work")
		o.SampleInterval = 100 * time.Millisecond
		r := New(o)
		h := &fakeHost{}
		r.host = h
		run, err := r.Start(context.Background(), testSpec("slow-samples", machine.R6, 600*time.Millisecond))
		if err != nil {
			t.Fatal(err)
		}
		release := make(chan struct{})
		f := &fakeSampleFile{syncFn: func() error { <-release; return nil }}
		run.(*running).openSamples = func(string) (sampleFile, error) { return f, nil }
		done := make(chan struct{})
		var result machine.Result
		go func() {
			result, err = run.Wait(context.Background(), &recorder{})
			close(done)
		}()
		time.Sleep(700 * time.Millisecond)
		synctest.Wait()
		wantSignals := []fakeSignal{
			{1000, syscall.SIGSTOP},
			{1000, syscall.SIGCONT},
			{1000, syscall.SIGSTOP},
			{1000, syscall.SIGCONT},
			{1000, syscall.SIGTERM},
			{1000, syscall.SIGKILL},
		}
		if diff := cmp.Diff(wantSignals, h.recordedSignals(), cmp.AllowUnexported(fakeSignal{})); diff != "" {
			t.Error(diff)
		}
		select {
		case <-done:
			t.Error("Wait returned before the pending sample was durable")
		default:
		}
		close(release)
		<-done
		if err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff(600*time.Millisecond, result.Ran); diff != "" {
			t.Fatal(diff)
		}
		want := []machine.TrialConditions{{ElapsedMS: 100}, {ElapsedMS: 200}}
		if diff := cmp.Diff(want, f.samples); diff != "" {
			t.Fatal(diff)
		}
		if diff := cmp.Diff(len(want), f.syncs); diff != "" {
			t.Fatal(diff)
		}
		if !f.closed {
			t.Fatal("samples file remained open")
		}
	})
}
