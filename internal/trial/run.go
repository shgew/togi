package trial

import (
	"context"
	"errors"
	"fmt"
	"iter"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"syscall"
	"time"

	"github.com/shgew/togi/internal/backend"
	"github.com/shgew/togi/internal/machine"
)

type watchFile struct {
	path   string
	offset int64
	size   int64
	lines  outputLines
	err    error
}
type cpuSample struct {
	active time.Duration
	cpu    time.Duration
}

func (t *running) Wait(ctx context.Context, report machine.Reporter) (result machine.Result, err error) {
	started := time.Now()
	watchCtx, cancelWatch := context.WithDeadline(ctx, started.Add(t.spec.Duration))
	defer cancelWatch()
	pendingSamples := make(chan machine.TrialConditions, 1)
	sampleErrors := make(chan error, 1)
	samplesDone := make(chan error, 1)
	go func() {
		samples, writeErr := t.openSamples(filepath.Join(t.options.Dir, t.spec.ID))
		if writeErr != nil {
			sampleErrors <- writeErr
			samplesDone <- writeErr
			return
		}
		for sample := range pendingSamples {
			if writeErr = appendSample(samples, sample); writeErr != nil {
				sampleErrors <- writeErr
				break
			}
		}
		if closeErr := samples.Close(); closeErr != nil {
			writeErr = errors.Join(writeErr, fmt.Errorf("close trial samples: %w", closeErr))
		}
		samplesDone <- writeErr
	}()
	defer func() {
		close(pendingSamples)
		err = errors.Join(err, <-samplesDone)
	}()
	conditions := newConditionsSampler(t.options, t.spec, started)
	result.Stops = t.initialStops
	watching := false
	for _, inst := range t.instances {
		watching = watching || len(inst.watch) > 0
		if !inst.suspended {
			inst.resumed = started
		}
	}
	deadline := time.NewTimer(t.spec.Duration)
	defer deadline.Stop()
	ticker := time.NewTicker(t.options.SampleInterval)
	defer ticker.Stop()
	next, stopPlan := iter.Pull(plan(t.spec, t.options.Cores))
	defer stopPlan()
	step, ok := next()
	var timer *time.Timer
	var signal <-chan time.Time
	setTimer := func() {
		if timer != nil {
			timer.Stop()
		}
		signal = nil
		if ok {
			delay := time.Until(started.Add(step.At))
			timer = time.NewTimer(max(0, delay))
			signal = timer.C
		}
	}
	setTimer()
	pollContext := func() (context.Context, context.CancelFunc) {
		if !watching {
			return watchCtx, func() {}
		}
		until := time.Now().Add(t.options.SampleInterval)
		if ok {
			until = minTime(until, started.Add(step.At))
		}
		return context.WithDeadline(watchCtx, until)
	}
	nextInstance := 0
	var decision bool
	var fatal error
	var r6start bool
	for !decision {
		if cause := contextError(ctx); cause != nil {
			err = cause
			break
		}
		if contextError(watchCtx) != nil {
			break
		}
		select {
		case <-ctx.Done():
			err = ctx.Err()
			decision = true
		case <-deadline.C:
			decision = true
		case <-ticker.C:
			pollCtx, cancelPoll := pollContext()
			firstInstance := nextInstance
			for i := range len(t.instances) {
				index := (firstInstance + i) % len(t.instances)
				inst := t.instances[index]
				if contextError(watchCtx) != nil {
					decision = true
					break
				}
				if contextError(pollCtx) == nil {
					nextInstance = (index + 1) % len(t.instances)
					if errorLine, _, _ := t.tail(pollCtx, inst, &result, report, false); errorLine {
						decision = true
						break
					}
				}
				if contextError(watchCtx) != nil {
					decision = true
					break
				}
				if !inst.ready && t.inScope(inst) {
					inst.ready = true
				}
				if !inst.ready || (inst.suspended && t.options.NoScope) {
					continue
				}
				if cpu, tid, escaped := t.outsideCPU(inst, &result); escaped {
					result.Escaped = []int{cpu}
					report.Sample(machine.Sample{Warning: "outside allowed cpus", PID: inst.PID, TID: tid, CPU: cpu})
					decision = true
					break
				}
				if t.sample(inst, time.Now(), &result, report) {
					decision = true
					break
				}
			}
			cancelPoll()
			sample := conditions.sample(started)
			if temp := sample.TctlC; temp != nil && (result.TctlMaxC == nil || *temp > *result.TctlMaxC) {
				result.TctlMaxC = temp
			}
			select {
			case pendingSamples <- sample:
			default:
			}
		case <-sampleErrors:
			decision = true
		case <-signal:
			now := time.Now()
			if t.spec.Regime == machine.R6 && !r6start && step.At >= t.spec.Duration/2 {
				r6start = true
				report.Progress("first half idle, then 100ms bursts every 2s, one core at a time")
			}
			for _, idx := range step.Instances {
				inst := t.instances[idx]
				if err := t.toggle(inst, step.Stop, now); err != nil {
					fatal = err
					decision = true
					break
				}
				if step.Stop {
					result.Stops++
				} else {
					result.Conts++
				}
			}
			if !decision {
				step, ok = next()
				setTimer()
			}
		case e := <-t.events:
			if cause := contextError(ctx); cause != nil {
				err = cause
				decision = true
				if e.exit {
					t.instances[e.index].done = true
				}
				break
			}
			if e.exit {
				pollCtx, cancelPoll := pollContext()
				decision = t.handleEvent(pollCtx, e, &result, report, true)
				cancelPoll()
			} else if t.handleEvent(watchCtx, e, &result, report, true) {
				decision = true
			}
		}
	}
	if err == nil {
		err = contextError(ctx)
	}
	t.drainEvents(watchCtx, &result, report, contextError(ctx) == nil)
	if !t.options.NoScope {
		for _, inst := range t.instances {
			if !inst.ready && !t.inScope(inst) && result.Signal == "" && len(result.Escaped) == 0 && result.Inconclusive == "" {
				result.Inconclusive = fmt.Sprintf("core %02d setup error: never entered scope %s", inst.Core, inst.Scope)
			}
		}
	}
	result.Ran = min(time.Since(started), t.spec.Duration)
	if timer != nil {
		timer.Stop()
	}
	if t.spec.Regime == machine.R6 {
		report.Progress(fmt.Sprintf("bursts: %d continues, %d stops", result.Conts, result.Stops))
	}
	err = errors.Join(err, t.teardown(&result, report), fatal)
	return result, err
}

func (t *running) classify(inst *instance, line string, stderr bool, result *machine.Result, report machine.Reporter) bool {
	classified := t.backend.Classify(line)
	if stderr && (strings.HasPrefix(line, "Failed to start transient scope unit") || strings.HasPrefix(line, "Failed to execute")) {
		classified = backend.Line{Kind: backend.SetupError, Detail: "systemd-run: " + line}
	}
	return t.classifyLine(inst, line, classified, stderr, result, report)
}

func (t *running) classifyLine(inst *instance, line string, classified backend.Line, stderr bool, result *machine.Result, report machine.Reporter) bool {
	switch classified.Kind {
	case backend.Progress:
		if !stderr {
			detail := classified.Detail
			if len(t.instances) > 1 {
				detail = fmt.Sprintf("core %02d %s", inst.Core, detail)
			}
			report.Progress(detail)
		}
	case backend.ComputationError:
		if len(result.Escaped) == 0 && result.Signal != machine.ComputationError {
			result.Signal = machine.ComputationError
			result.Core = inst.Core
			result.Inconclusive = ""
			report.Signal(inst.Core, machine.ComputationError, line)
		}
		return true
	case backend.SetupError:
		inst.setup = true
		if len(result.Escaped) == 0 && result.Signal == "" && result.Inconclusive == "" {
			result.Inconclusive = fmt.Sprintf("%s setup error: %s", t.backend.Name(), line)
		}
		return true
	case backend.AffinityError:
		if len(result.Escaped) == 0 {
			result.Escaped = []int{classified.CPU}
			result.Signal = ""
			result.Inconclusive = ""
			report.Sample(machine.Sample{Warning: "backend could not set affinity", PID: inst.PID, TID: inst.PID, CPU: classified.CPU})
		}
		return true
	case backend.Other:
	}
	return false
}

func (t *running) tail(ctx context.Context, inst *instance, result *machine.Result, report machine.Reporter, finalize bool) (found, drained, backlogProgress bool) {
	drained = true
	exitReady := true
	for range len(inst.watch) {
		if contextError(ctx) != nil {
			return found, false, backlogProgress
		}
		w := &inst.watch[inst.watchNext]
		inst.watchNext = (inst.watchNext + 1) % len(inst.watch)
		offset := w.offset
		complete := false
		var err error
		if w.err != nil {
			complete = w.lines.exceeded || errors.Is(w.err, errWatchedOutputRejected)
		} else {
			complete, err = w.read(ctx, func(line string) {
				if t.classifyWatch(inst, line, result, report) {
					found = true
				}
			})
			if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
				return found, false, backlogProgress
			}
			if errors.Is(err, os.ErrNotExist) && w.offset >= w.size {
				complete = true
				err = nil
			}
			if err != nil {
				w.err = fmt.Errorf("read watched file %s: %w", w.path, err)
				t.outputError(w.err, result)
				if errors.Is(err, errOutputLineTooLong) || errors.Is(err, errWatchedOutputRejected) {
					complete = true
				}
				found = true
			}
		}
		drained = drained && complete
		backlogProgress = backlogProgress || (!complete && err == nil && w.offset > offset)
		if finalize && complete && inst.done && !w.lines.exceeded && len(w.lines.pending) > 0 {
			if contextError(ctx) != nil {
				return found, false, backlogProgress
			}
			if t.classifyWatch(inst, string(w.lines.pending), result, report) {
				found = true
			}
			w.lines.pending = nil
		}
		if !w.lines.exceeded && len(w.lines.pending) > 0 {
			exitReady = false
		}
	}
	if contextError(ctx) != nil {
		return found, false, backlogProgress
	}
	if drained && inst.done && exitReady && t.classifyExit(inst, result, report) {
		found = true
	}
	return found, drained, backlogProgress
}
func (t *running) classifyWatch(inst *instance, line string, result *machine.Result, report machine.Reporter) bool {
	classified := t.backend.Classify(line)
	switch classified.Kind {
	case backend.ComputationError, backend.SetupError, backend.AffinityError:
		return t.classifyLine(inst, line, classified, false, result, report)
	case backend.Other, backend.Progress:
	}
	return false
}

func (t *running) outputError(err error, result *machine.Result) {
	if errors.Is(err, errOutputLineTooLong) {
		t.outputCapErr = errors.Join(t.outputCapErr, err)
	} else {
		t.outputErr = errors.Join(t.outputErr, err)
	}
	if result.Inconclusive == "" && result.Signal == "" && len(result.Escaped) == 0 {
		result.Inconclusive = err.Error()
	}
}

func (t *running) toggle(inst *instance, stop bool, now time.Time) error {
	sig := syscall.SIGCONT
	if stop {
		sig = syscall.SIGSTOP
	}
	if err := t.host.SignalGroup(inst.process, sig); err != nil && !errors.Is(err, syscall.ESRCH) {
		return fmt.Errorf("signal core %02d: %w", inst.Core, err)
	}
	if stop && !inst.suspended {
		inst.active += now.Sub(inst.resumed)
		inst.suspended = true
	}
	if !stop && inst.suspended {
		inst.resumed = now
		inst.suspended = false
	}
	return nil
}

func (t *running) handleEvent(ctx context.Context, e streamEvent, result *machine.Result, report machine.Reporter, unexpected bool) bool {
	inst := t.instances[e.index]
	if e.exit {
		inst.done = true
		inst.unexpected = unexpected
		inst.exitErr = e.err
		t.classifyPartial(inst, result, report)
		found, _, _ := t.tail(ctx, inst, result, report, false)
		return found || unexpected
	}
	if e.err != nil {
		t.outputError(e.err, result)
		return true
	}
	if !e.eof && e.err == nil {
		return t.classify(inst, e.line, e.stderr, result, report)
	}
	return false
}

func (t *running) classifyExit(inst *instance, result *machine.Result, report machine.Reporter) bool {
	if !inst.unexpected {
		return false
	}
	inst.unexpected = false
	if t.outputErr != nil || t.outputCapErr != nil || inst.setup || len(result.Escaped) != 0 || result.Signal != "" {
		return false
	}
	result.Signal = machine.UnexpectedExit
	result.Core = inst.Core
	result.Inconclusive = ""
	status := "exit status 0"
	if inst.exitErr != nil {
		status = inst.exitErr.Error()
	}
	report.Signal(inst.Core, machine.UnexpectedExit, status)
	return true
}

func (t *running) drainEvents(ctx context.Context, result *machine.Result, report machine.Reporter, unexpected bool) {
	for range len(t.events) {
		e := <-t.events
		t.handleEvent(ctx, e, result, report, unexpected)
	}
}

func (t *running) Stop() error {
	return t.teardown(&machine.Result{}, discardReport{})
}

func (t *running) teardown(result *machine.Result, report machine.Reporter) error {
	t.stopMu.Lock()
	defer t.stopMu.Unlock()
	if t.stopped {
		return t.cleanupErr
	}
	if t.cancel != nil {
		defer t.cancel()
	}
	deadline := time.Now().Add(teardownLimit)
	ctx, cancel := context.WithDeadline(context.Background(), deadline)
	defer cancel()
	cleanupErr := terminate(t.host, t.instances, t.scopes, deadline, t.options.StopGrace, false, func(until time.Time, final bool) bool {
		return t.collect(until, result, report, final)
	})
	if t.streamStop != nil && !t.streamClosed {
		t.streamClosed = true
		close(t.streamStop)
	}
	for _, inst := range t.instances {
		if inst.process != nil {
			inst.process.Stdout().Close()
			inst.process.Stderr().Close()
		}
	}
	t.streams.Wait()
	for _, inst := range t.instances {
		select {
		case <-inst.reaped:
			<-inst.joined
		default:
		}
	}
	t.drainEvents(ctx, result, report, false)
	for _, inst := range t.instances {
		t.classifyPartial(inst, result, report)
	}
	if cleanupErr != nil {
		cleanupErr = errors.Join(machine.ErrContainment, cleanupErr)
	}
	t.cleanupErr = errors.Join(cleanupErr, t.outputErr)
	if len(result.Escaped) == 0 && result.Signal == "" {
		t.cleanupErr = errors.Join(t.cleanupErr, t.outputCapErr)
	}
	t.stopped = true
	return t.cleanupErr
}

func (t *running) classifyPartial(inst *instance, result *machine.Result, report machine.Reporter) {
	for stream, line := range inst.partial {
		if line != "" {
			t.classify(inst, line, stream == 1, result, report)
			inst.partial[stream] = ""
		}
	}
}
func (t *running) collect(deadline time.Time, result *machine.Result, report machine.Reporter, final bool) bool {
	ctx, cancel := context.WithDeadline(context.Background(), deadline)
	defer cancel()
	ticker := time.NewTicker(t.options.SampleInterval)
	defer ticker.Stop()
	for {
		if ctx.Err() != nil || !time.Now().Before(deadline) {
			return false
		}
		t.drainEvents(ctx, result, report, false)
		complete := true
		watchFinalized := true
		backlogProgress := false
		for _, inst := range t.instances {
			_, drained, progressed := t.tail(ctx, inst, result, report, final && inst.writersStopped)
			backlogProgress = backlogProgress || progressed
			if !inst.done || !drained {
				complete = false
			}
			if final && !inst.writersStopped {
				for i := range inst.watch {
					w := &inst.watch[i]
					if !w.lines.exceeded && len(w.lines.pending) > 0 {
						watchFinalized = false
					}
				}
			}
		}
		if ctx.Err() != nil || !time.Now().Before(deadline) {
			return false
		}
		if complete {
			return watchFinalized
		}
		if backlogProgress {
			continue
		}
		select {
		case <-ctx.Done():
			return false
		case <-ticker.C:
		case e := <-t.events:
			t.handleEvent(ctx, e, result, report, false)
		}
	}
}
func (t *running) abort(err error) error {
	return errors.Join(err, t.teardown(&machine.Result{}, discardReport{}))
}

type discardReport struct{}

func (discardReport) Progress(string)                    {}
func (discardReport) Sample(machine.Sample)              {}
func (discardReport) Signal(int, machine.Signal, string) {}

func (t *running) inScope(inst *instance) bool {
	return t.options.NoScope || t.host.InScope(inst.PID, inst.Scope)
}

func processDisappeared(err error) bool {
	return errors.Is(err, os.ErrNotExist) || errors.Is(err, syscall.ESRCH)
}

func (t *running) outsideCPU(inst *instance, result *machine.Result) (cpu, tid int, escaped bool) {
	threads, err := t.host.Threads(inst.PID)
	if err != nil && !processDisappeared(err) && result.Inconclusive == "" {
		result.Inconclusive = fmt.Sprintf("core %02d thread sampling lost: %v", inst.Core, err)
	}
	for _, task := range threads {
		if !slices.Contains(inst.CPUs, task.CPU) {
			return task.CPU, task.TID, true
		}
	}
	return 0, 0, false
}
func (t *running) sample(inst *instance, now time.Time, result *machine.Result, report machine.Reporter) bool {
	reading, err := t.host.Usage(inst.PID)
	if err != nil {
		if !processDisappeared(err) && result.Inconclusive == "" {
			result.Inconclusive = fmt.Sprintf("core %02d usage sampling lost: %v", inst.Core, err)
		}
		return false
	}
	active := inst.active
	if !inst.suspended {
		active += now.Sub(inst.resumed)
	}
	cpu := reading.CPUTime
	current := cpuSample{active: active, cpu: cpu}
	inst.samples = append(inst.samples, current)
	if active < t.options.StallGrace {
		return false
	}
	latest := -1
	for i, earlier := range slices.Backward(inst.samples) {
		if earlier.active < t.options.StallGrace {
			break
		}
		if active-earlier.active >= t.options.StallWindow {
			latest = i
			break
		}
	}
	if latest < 0 {
		return false
	}
	earlier := inst.samples[latest]
	inst.samples = inst.samples[latest:]
	if stalled(earlier, current, len(inst.CPUs)) {
		span := active - earlier.active
		used := cpu - earlier.cpu
		result.Signal = machine.Stall
		result.Core = inst.Core
		detail := fmt.Sprintf("%.1fs cpu in %.1fs running", used.Seconds(), span.Seconds())
		report.Signal(inst.Core, machine.Stall, detail)
		report.Sample(machine.Sample{Warning: "stalled: " + detail, PID: inst.PID, TID: inst.PID, CPU: reading.CPU})
		return true
	}
	return false
}
func stalled(a, b cpuSample, threads int) bool {
	span := b.active - a.active
	return span > 0 && b.cpu-a.cpu < time.Duration(float64(span)*0.5*float64(threads))
}
