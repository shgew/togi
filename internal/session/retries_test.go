package session

import (
	"context"
	"errors"
	"fmt"
	"io"
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

func (k *unreadableKernel) ResetReasonAfter(boot string) (machine.ResetReason, error) {
	k.calls++
	if k.calls <= k.failUntil {
		return machine.ResetReason{}, fmt.Errorf("simulated unreadable kernel log")
	}
	return k.Kernel.ResetReasonAfter(boot)
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

type missingSuccessorKernel struct{ machine.Kernel }

func (k missingSuccessorKernel) ResetReasonAfter(boot string) (machine.ResetReason, error) {
	return machine.ResetReason{}, fmt.Errorf("find system boot after %s: %w", boot, machine.ErrBootMissing)
}

func TestRecoveryWithoutSuccessorDoesNotBorrowCurrentResetReason(t *testing.T) {
	t.Parallel()
	in, before := firstCrash(t, machine.ResetWatchdog, machine.Crash, false)
	in.Machine.NextReset(machine.ResetThermalTrip)
	in.Machine.Reboot()
	seams := in.Machine.Seams()
	seams.Kernel = missingSuccessorKernel{Kernel: seams.Kernel}
	stop, err := driveWithSeams(in, seams)
	if err != nil || stop.Reason != StopRotations {
		t.Fatalf("recover: %+v, %v", stop, err)
	}
	events := readEvents(t, in.Dir)
	crash, ok := crashDetectedFor(events, before[0].Boot)
	if !ok {
		t.Fatal("missing crash detection")
	}
	p := crash.Data.(*journal.CrashDetected)
	if p.ResetReason != "" || p.ResetReasonRaw != "" || p.Inconclusive {
		t.Fatalf("unidentified successor must leave ordinary crash handling: %+v", p)
	}
	for _, e := range events {
		if p, ok := e.Data.(*journal.BackendRetry); ok && p.Backend == "kernel_log" {
			t.Fatalf("missing successor retried: %s", e.Msg)
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
	in.Seams = &seams
	return runSim(context.Background(), in, nil)
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

type waitErrorTrials struct {
	machine.Trials
	cancel context.CancelFunc
}

func (t waitErrorTrials) Start(ctx context.Context, spec machine.TrialSpec) (machine.Running, error) {
	running, err := t.Trials.Start(ctx, spec)
	if err != nil {
		return nil, err
	}
	return waitErrorRunning{Running: running, cancel: t.cancel}, nil
}

type waitErrorRunning struct {
	machine.Running
	cancel context.CancelFunc
}

func (r waitErrorRunning) Wait(ctx context.Context, report machine.Reporter) (machine.Result, error) {
	stopCtx, stop := context.WithCancel(ctx)
	stop()
	result, _ := r.Running.Wait(stopCtx, report)
	if r.cancel != nil {
		r.cancel()
		return result, context.Canceled
	}
	return result, errors.New("injected runner failure")
}

func TestRunnerWaitErrorsCountTowardBackendDeadEnd(t *testing.T) {
	t.Parallel()
	r, _, closeJournal := checkedRunner(t, []int{0, 0})
	defer closeJournal()
	r.in.Config.DeadEnds.InconclusiveInARow = 1
	clock := &retryClock{Clock: r.in.Machine.Clock}
	r.in.Machine.Clock = clock
	r.in.Machine.Trials = waitErrorTrials{Trials: r.in.Machine.Trials}
	trial := tuner.Trial{Core: 0, Regime: machine.R1, Workload: "mprime-sse-4k-21k", Condition: machine.Isolated, DurationS: 90}
	for attempt := range 4 {
		if err := r.retryBackend(context.Background(), trial); err != nil {
			t.Fatal(err)
		}
		if err := r.trial(context.Background(), tuner.Action{Trial: trial}); err != nil {
			t.Fatal(err)
		}
		e := r.in.Journal.Events()[len(r.in.Journal.Events())-1]
		end, ok := e.Data.(*journal.TrialEnd)
		if !ok || end.Outcome != journal.OutcomeInconclusive || end.Interrupted || end.Reason != "trial runner failed: injected runner failure" {
			t.Fatalf("ordinary Wait error: %+v", e.Data)
		}
		if got := len(r.fold.streaks[machine.Mprime]); got != attempt+1 {
			t.Fatalf("backend streak %d, want %d", got, attempt+1)
		}
	}
	if diff := cmp.Diff([]time.Duration{time.Minute, 5 * time.Minute, 30 * time.Minute}, clock.waits); diff != "" {
		t.Fatal(diff)
	}
	stop, err := r.checkDeadEnd()
	if err != nil || stop == nil || stop.DeadEnd.Condition != journal.DeadEndNoEvidence {
		t.Fatalf("backend dead end: %+v, %v", stop, err)
	}
}

func TestRunnerWaitCancellationRemainsInterrupted(t *testing.T) {
	t.Parallel()
	r, _, closeJournal := checkedRunner(t, []int{0, 0})
	defer closeJournal()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	r.in.Machine.Trials = waitErrorTrials{Trials: r.in.Machine.Trials, cancel: cancel}
	trial := tuner.Trial{Core: 0, Regime: machine.R1, Condition: machine.Isolated, DurationS: 90}
	if err := r.trial(ctx, tuner.Action{Trial: trial}); err != nil {
		t.Fatal(err)
	}
	e := r.in.Journal.Events()[len(r.in.Journal.Events())-1]
	end, ok := e.Data.(*journal.TrialEnd)
	if !ok || end.Outcome != journal.OutcomeInconclusive || !end.Interrupted || end.Reason != "trial runner failed: context canceled; stopped by signal" {
		t.Fatalf("cancelled Wait: %+v", e.Data)
	}
	if got := r.fold.streaks[machine.Mprime]; len(got) != 0 {
		t.Fatalf("cancelled trial counted toward backend streak: %v", got)
	}
}

func TestBackendRetryCancellationStopsBeforeAnotherTrial(t *testing.T) {
	t.Parallel()
	r, m, closeJournal := checkedRunner(t, []int{0, 0})
	defer closeJournal()
	if err := r.startSession(); err != nil {
		t.Fatal(err)
	}
	r.in.Machine.Trials = waitErrorTrials{Trials: r.in.Machine.Trials}
	if err := r.trial(context.Background(), tuner.Action{Trial: tuner.Trial{Core: 0, Regime: machine.R1, Workload: "mprime-sse-4k-21k", Condition: machine.Isolated, DurationS: 90}}); err != nil {
		t.Fatal(err)
	}
	before := len(r.in.Journal.Events())
	r.in.Config.DeadEnds.InconclusiveInARow = 1
	r.in.Machine.Clock = cancelledClock{Clock: r.in.Machine.Clock}
	stop, err := r.loop(context.Background())
	if err != nil || stop.Reason != StopSignal {
		t.Fatalf("retry cancellation: %+v, %v", stop, err)
	}
	if err := r.close(true, &stop); err != nil {
		t.Fatal(err)
	}
	var retries []int
	for _, e := range r.in.Journal.Events()[before:] {
		if e.Kind == journal.KindTrialIntent {
			t.Fatal("retry cancellation started another trial")
		}
		if p, ok := e.Data.(*journal.BackendRetry); ok {
			retries = append(retries, p.WaitS)
		}
	}
	if diff := cmp.Diff([]int{60}, retries); diff != "" {
		t.Fatal(diff)
	}
	last := r.in.Journal.Events()[len(r.in.Journal.Events())-1].Data
	if shutdown, ok := last.(*journal.Shutdown); !ok || shutdown.Reason != journal.ShutdownSignal {
		t.Fatalf("retry cancellation did not shut down cleanly: %+v", last)
	}
	for core := range 2 {
		if offset, err := m.Seams().SMU.Offset(core); err != nil || offset != 0 {
			t.Fatalf("offset after retry cancellation: core %d, %d, %v", core, offset, err)
		}
	}
}

func TestRetryAppendFailurePreventsSleepAndTrial(t *testing.T) {
	t.Parallel()
	for _, kernel := range []bool{false, true} {
		t.Run(map[bool]string{false: "backend", true: "kernel"}[kernel], func(t *testing.T) {
			t.Parallel()
			r, _, closeJournal := checkedRunner(t, []int{0, 0})
			defer closeJournal()
			if !kernel {
				if err := r.startSession(); err != nil {
					t.Fatal(err)
				}
				r.in.Machine.Trials = waitErrorTrials{Trials: r.in.Machine.Trials}
				if err := r.trial(context.Background(), tuner.Action{Trial: tuner.Trial{Core: 0, Regime: machine.R1, Workload: "mprime-sse-4k-21k", Condition: machine.Isolated, DurationS: 90}}); err != nil {
					t.Fatal(err)
				}
			}
			clock := &retryClock{Clock: r.in.Machine.Clock}
			r.in.Machine.Clock = clock
			r.in.Journal = &failAppendJournal{Journal: r.in.Journal, kind: journal.KindBackendRetry}
			before := r.in.Journal.Events()
			var err error
			if kernel {
				err = r.retryKernel(context.Background(), "older", errors.New("unreadable"))
			} else {
				r.in.Config.DeadEnds.InconclusiveInARow = 1
				err = r.retryBackend(context.Background(), tuner.Trial{Core: 0, Regime: machine.R1})
			}
			if !errors.Is(err, io.ErrClosedPipe) {
				t.Fatalf("retry journal failure: %v", err)
			}
			if diff := cmp.Diff([]time.Duration(nil), clock.waits); diff != "" {
				t.Fatalf("wait without durable intent:\n%s", diff)
			}
			if diff := cmp.Diff(before, r.in.Journal.Events()); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestPendingKernelRetryCancellationKeepsObservationPending(t *testing.T) {
	t.Parallel()
	for _, reason := range []bool{false, true} {
		t.Run(map[bool]string{false: "MCEs", true: "reset reason"}[reason], func(t *testing.T) {
			t.Parallel()
			r, _, closeJournal := checkedRunner(t, []int{0, 0})
			defer closeJournal()
			if _, err := r.append(&journal.BackendRetry{Backend: "kernel_log", Attempt: 1, WaitS: 60, Reason: "unavailable"}); err != nil {
				t.Fatal(err)
			}
			before := r.in.Journal.Events()
			kernel := &unreadableKernel{Kernel: r.in.Machine.Kernel, failUntil: 10}
			r.in.Machine.Kernel = kernel
			r.in.Machine.Clock = cancelledClock{Clock: r.in.Machine.Clock}
			var err error
			if reason {
				_, err = r.readResetReason(context.Background(), "previous", true)
			} else {
				_, err = r.readMCEs(context.Background(), "previous")
			}
			if !errors.Is(err, context.Canceled) || kernel.calls != 0 {
				t.Fatalf("canceled pending observation: %v, reads %d", err, kernel.calls)
			}
			if diff := cmp.Diff(before, r.in.Journal.Events()); diff != "" {
				t.Fatalf("cancellation consumed or replaced pending retry:\n%s", diff)
			}
		})
	}
}
