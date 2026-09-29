package session

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

type retryClock struct {
	machine.Clock
	waits    []time.Duration
	failNext bool
}

func (c *retryClock) Sleep(ctx context.Context, d time.Duration) error {
	c.waits = append(c.waits, d)
	if c.failNext {
		c.failNext = false
		return machine.ErrCrashed
	}
	return c.Clock.Sleep(ctx, d)
}

func TestBackendRetriesAtOneFiveThirtyMinutesAndResumeWait(t *testing.T) {
	t.Parallel()
	r, _, closeJournal := checkedRunner(t, []int{0, 0, 0, 0})
	defer closeJournal()
	clock := &retryClock{Clock: r.in.Machine.Clock}
	r.in.Machine.Clock = clock
	r.in.Config.DeadEnds.InconclusiveInARow = 1
	trial := tuner.Trial{Core: 0, Regime: machine.R1, Workload: "mprime-sse-4k-21k"}
	var retries []journal.BackendRetry
	for attempt, wait := range []int{60, 300, 1800} {
		r.fold.streaks[machine.Mprime] = make([]int, attempt+1)
		r.fold.lastReason[machine.Mprime] = "setup failed"
		if attempt == 0 {
			clock.failNext = true
		}
		err := r.retryBackend(context.Background(), trial)
		if attempt == 0 {
			if !errors.Is(err, machine.ErrCrashed) {
				t.Fatalf("interrupted retry: %v", err)
			}
			if err := r.retryBackend(context.Background(), trial); err != nil {
				t.Fatalf("resume same retry: %v", err)
			}
		} else if err != nil {
			t.Fatalf("attempt %d: %v", attempt+1, err)
		}
		for _, e := range r.in.Journal.Events() {
			if p, ok := e.Data.(*journal.BackendRetry); ok && p.Backend == string(machine.Mprime) && p.Attempt == attempt+1 {
				retries = append(retries, *p)
			}
		}
		if len(retries) != attempt+1 || retries[attempt].WaitS != wait || retries[attempt].Attempt != attempt+1 {
			t.Fatalf("retry %d: %+v", attempt+1, retries)
		}
	}
	if diff := cmp.Diff([]time.Duration{time.Minute, time.Minute, 5 * time.Minute, 30 * time.Minute}, clock.waits); diff != "" {
		t.Fatalf("retry waits (-want +got):\n%s", diff)
	}
	if got := len(r.in.Journal.Events()); got == 0 {
		t.Fatal("no journal events recorded")
	}
}

type unreadableKernel struct {
	machine.Kernel
	calls     int
	failUntil int
}

func (k *unreadableKernel) MCEs(boot string, since time.Duration) ([]machine.MCE, error) {
	k.calls++
	if k.calls <= k.failUntil {
		return nil, fmt.Errorf("simulated unreadable kernel log")
	}
	return k.Kernel.MCEs(boot, since)
}
func (k *unreadableKernel) ResetReason(boot string) (machine.ResetReason, error) {
	k.calls++
	if k.calls <= k.failUntil {
		return machine.ResetReason{}, fmt.Errorf("simulated unreadable kernel log")
	}
	return k.Kernel.ResetReason(boot)
}

func TestRecoveryKernelLogRetriesAndDeadEnd(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name      string
		failUntil int
		deadEnd   bool
	}{
		{"read recovers after three retries", 3, false},
		{"unreadable after three retries", 100, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			in, _ := firstCrash(t, machine.ResetWatchdog, machine.Crash, false)
			seams := in.Machine.Seams()
			kernel := &unreadableKernel{Kernel: seams.Kernel, failUntil: tc.failUntil}
			seams.Kernel = kernel
			clock := &retryClock{Clock: seams.Clock}
			seams.Clock = clock
			stop, err := driveWithSeams(in, seams)
			if err != nil {
				t.Fatalf("recover: %v", err)
			}
			if tc.deadEnd && (stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != journal.DeadEndNoEvidence) {
				t.Fatalf("stop %+v", stop)
			}
			if !tc.deadEnd && stop.Reason != StopRotations {
				t.Fatalf("stop %+v", stop)
			}
			var got, retrySeqs, deadEndCause []int
			for _, e := range readEvents(t, in.Dir) {
				if p, ok := e.Data.(*journal.BackendRetry); ok && p.Backend == "kernel_log" {
					got = append(got, p.WaitS)
					retrySeqs = append(retrySeqs, e.Seq)
				}
				if p, ok := e.Data.(*journal.DeadEnd); ok && p.Condition == journal.DeadEndNoEvidence {
					deadEndCause = e.Cause
				}
			}
			if diff := cmp.Diff([]int{60, 300, 1800}, got); diff != "" {
				t.Fatalf("kernel retry waits (-want +got):\n%s", diff)
			}
			if tc.deadEnd {
				if diff := cmp.Diff(retrySeqs, deadEndCause); diff != "" {
					t.Fatalf("kernel dead end cause (-want +got):\n%s", diff)
				}
			}
			if diff := cmp.Diff([]time.Duration{time.Minute, 5 * time.Minute, 30 * time.Minute}, clock.waits); diff != "" {
				t.Fatalf("sleep waits (-want +got):\n%s", diff)
			}
		})
	}
}

type cancelledClock struct{ machine.Clock }

func (cancelledClock) Sleep(context.Context, time.Duration) error { return context.Canceled }

func TestRecoveryKernelLogRetryStopsCleanlyOnSignal(t *testing.T) {
	t.Parallel()
	in, _ := firstCrash(t, machine.ResetWatchdog, machine.Crash, false)
	seams := in.Machine.Seams()
	readable := seams.Kernel
	seams.Kernel = &unreadableKernel{Kernel: readable, failUntil: 1}
	seams.Clock = cancelledClock{seams.Clock}
	stop, err := runWithSeams(context.Background(), in, seams)
	if err != nil || stop.Reason != StopSignal {
		t.Fatalf("signal during kernel retry wait: %+v %v", stop, err)
	}
	events := readEvents(t, in.Dir)
	if p, ok := events[len(events)-1].Data.(*journal.Shutdown); !ok || p.Reason != journal.ShutdownSignal {
		t.Fatalf("last event %s, want signal shutdown", events[len(events)-1].Msg)
	}
	seams = in.Machine.Seams()
	if stop, err := driveWithSeams(in, seams); err != nil || stop.Reason != StopRotations {
		t.Fatalf("resume: %+v %v", stop, err)
	}
	for _, e := range readEvents(t, in.Dir) {
		if p, ok := e.Data.(*journal.CrashDetected); ok && p.Stray {
			t.Fatalf("interrupted recovery counted as a stray crash: %s", e.Msg)
		}
	}
}

type vacuumedKernel struct {
	machine.Kernel
	gone string
}

func (k vacuumedKernel) MCEs(boot string, since time.Duration) ([]machine.MCE, error) {
	if boot == k.gone {
		return nil, fmt.Errorf("read kernel log of boot %s: %w", boot, machine.ErrBootMissing)
	}
	return k.Kernel.MCEs(boot, since)
}

func (k vacuumedKernel) ResetReason(boot string) (machine.ResetReason, error) {
	if boot == k.gone {
		return machine.ResetReason{}, fmt.Errorf("read kernel log of boot %s: %w", boot, machine.ErrBootMissing)
	}
	return k.Kernel.ResetReason(boot)
}

func TestRecoverySkipsAnOlderBootTheSystemJournalDropped(t *testing.T) {
	t.Parallel()
	in, events := firstCrash(t, machine.ResetWatchdog, machine.Crash, false)
	seams := in.Machine.Seams()
	seams.Kernel = vacuumedKernel{Kernel: seams.Kernel, gone: events[0].Boot}
	stop, err := driveWithSeams(in, seams)
	if err != nil {
		t.Fatalf("recover: %v", err)
	}
	if stop.Reason != StopRotations {
		t.Fatalf("stop %+v, want rotations", stop)
	}
	for _, e := range readEvents(t, in.Dir) {
		if p, ok := e.Data.(*journal.BackendRetry); ok && p.Backend == "kernel_log" {
			t.Fatalf("kernel log retried for a dropped older boot: %s", e.Msg)
		}
	}
}

func TestMissingBackendDeadEndsWithoutRetries(t *testing.T) {
	t.Parallel()
	in := simInput(t.TempDir(), newSim(t, small()))
	in.Machine.MissBackend(machine.Mprime)
	stop := simulate(t, in)
	if stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != journal.DeadEndNoEvidence {
		t.Fatalf("missing backend stopped with %+v", stop)
	}
	var missing bool
	for _, e := range readEvents(t, in.Dir) {
		if p, ok := e.Data.(*journal.TrialEnd); ok && p.BackendMissing {
			missing = true
		}
		if e.Kind == journal.KindBackendRetry {
			t.Fatalf("retry after missing backend: %s", e.Msg)
		}
	}
	if !missing {
		t.Fatal("no trial.end marked backend missing")
	}
}

func TestRecoveryRetryResumeUsesSameAttempt(t *testing.T) {
	t.Parallel()
	in, _ := firstCrash(t, machine.ResetWatchdog, machine.Crash, false)
	seams := in.Machine.Seams()
	clock := &retryClock{Clock: seams.Clock, failNext: true}
	seams.Clock = clock
	kernel := &unreadableKernel{Kernel: seams.Kernel, failUntil: 1}
	seams.Kernel = kernel
	_, err := runWithSeams(context.Background(), in, seams)
	if !errors.Is(err, machine.ErrCrashed) {
		t.Fatalf("mid-wait crash: %v", err)
	}
	if diff := cmp.Diff([]time.Duration{time.Minute}, clock.waits); diff != "" {
		t.Fatalf("first wait (-want +got):\n%s", diff)
	}
	in.Machine.Crash()
	in.Machine.Reboot()
	seams = in.Machine.Seams()
	clock.Clock = seams.Clock
	seams.Clock = clock
	seams.Kernel = kernel
	stop, err := driveWithSeams(in, seams)
	if err != nil || stop.Reason != StopRotations {
		t.Fatalf("resumed recovery %+v, %v", stop, err)
	}
	var attempts []int
	for _, e := range readEvents(t, in.Dir) {
		if p, ok := e.Data.(*journal.BackendRetry); ok && p.Backend == "kernel_log" {
			attempts = append(attempts, p.Attempt)
		}
	}
	if diff := cmp.Diff([]int{1}, attempts); diff != "" {
		t.Fatalf("retry attempts (-want +got):\n%s", diff)
	}
	if !slices.Equal(clock.waits, []time.Duration{time.Minute, time.Minute}) {
		t.Fatalf("waits %v, want full same attempt twice", clock.waits)
	}
}

func driveWithSeams(in simRun, seams machine.Machine) (Stop, error) {
	for range maxSimulatedBoots {
		stop, err := runWithSeams(context.Background(), in, seams)
		if !errors.Is(err, machine.ErrCrashed) {
			return stop, err
		}
		in.Machine.Reboot()
	}
	return Stop{}, errors.New("too many boots")
}

type failingBackendTrials struct {
	machine.Trials
	remaining int
}

func (t *failingBackendTrials) Start(ctx context.Context, spec machine.TrialSpec) (machine.Running, error) {
	if spec.Workload.Backend == machine.Mprime && t.remaining > 0 {
		t.remaining--
		return nil, errors.New("simulated backend setup failure")
	}
	return t.Trials.Start(ctx, spec)
}

func TestBackendRetriesAfterInconclusiveTrials(t *testing.T) {
	t.Parallel()
	in := simInput(t.TempDir(), newSim(t, small()))
	in.Config.DeadEnds.InconclusiveInARow = 1
	seams := in.Machine.Seams()
	failing := &failingBackendTrials{Trials: seams.Trials, remaining: 4}
	seams.Trials = failing
	stop, err := driveWithSeams(in, seams)
	if err != nil || stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != journal.DeadEndNoEvidence {
		t.Fatalf("backend retries %+v, %v", stop, err)
	}
	if failing.remaining != 0 {
		t.Fatalf("%d setup failures not exercised", failing.remaining)
	}
	var retries []int
	var failures int
	for _, e := range readEvents(t, in.Dir) {
		if p, ok := e.Data.(*journal.BackendRetry); ok && p.Backend == string(machine.Mprime) {
			retries = append(retries, p.WaitS)
		}
		if p, ok := e.Data.(*journal.TrialEnd); ok && p.Outcome == journal.OutcomeInconclusive && p.Reason != "" {
			failures++
		}
	}
	if diff := cmp.Diff([]int{60, 300, 1800}, retries); diff != "" {
		t.Fatalf("backend waits (-want +got):\\n%s", diff)
	}
	if failures < 4 {
		t.Fatalf("%d inconclusive trials, want four backend setup failures", failures)
	}
}
