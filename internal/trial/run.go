package trial

import (
	"context"
	"errors"
	"fmt"
	"io"
	"iter"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/shgew/togi/internal/backend"
	"github.com/shgew/togi/internal/machine"
)

type watchFile struct {
	path    string
	offset  int64
	pending string
}
type cpuSample struct {
	active time.Duration
	cpu    time.Duration
}

func (t *running) Wait(ctx context.Context, report machine.Reporter) (result machine.Result, err error) {
	started := time.Now()
	result.Stops = t.initialStops
	for _, inst := range t.instances {
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
	var decision bool
	var fatal error
	var r6start bool
	for !decision {
		select {
		case <-ctx.Done():
			err = ctx.Err()
			decision = true
		case <-deadline.C:
			decision = true
		case <-ticker.C:
			for _, inst := range t.instances {
				if errorLine := t.tail(inst, &result, report); errorLine {
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
			if temp := readTctl(t.options.Hwmon); temp != nil && (result.TctlMaxC == nil || *temp > *result.TctlMaxC) {
				result.TctlMaxC = temp
			}
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
			if cause := ctx.Err(); cause != nil {
				err = cause
				decision = true
				if e.exit {
					t.instances[e.index].done = true
				}
				break
			}
			if e.err != nil && !e.exit {
				fatal = e.err
				decision = true
				break
			}
			if t.handleEvent(e, &result, report, true) {
				decision = true
			}
		}
	}
	t.drainEvents(&result, report, ctx.Err() == nil)
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
	if cleanupErr := t.teardown(&result, report); cleanupErr != nil && err == nil {
		err = cleanupErr
	}
	if fatal != nil && err == nil {
		err = fatal
	}
	return result, err
}

func (t *running) classify(inst *instance, line string, stderr bool, result *machine.Result, report machine.Reporter) bool {
	classified := t.backend.Classify(line)
	if stderr && (strings.HasPrefix(line, "Failed to start transient scope unit") || strings.HasPrefix(line, "Failed to execute")) {
		classified = backend.Line{Kind: backend.SetupError, Detail: "systemd-run: " + line}
	}
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

func (t *running) tail(inst *instance, result *machine.Result, report machine.Reporter) bool {
	var found bool
	for i := range inst.watch {
		w := &inst.watch[i]
		f, err := os.Open(w.path)
		if os.IsNotExist(err) {
			continue
		}
		if err != nil {
			if result.Inconclusive == "" && result.Signal == "" && len(result.Escaped) == 0 {
				result.Inconclusive = fmt.Sprintf("read watched file %s: %v", w.path, err)
			}
			found = true
			continue
		}
		if _, err = f.Seek(w.offset, io.SeekStart); err != nil {
			f.Close()
			if result.Inconclusive == "" && result.Signal == "" && len(result.Escaped) == 0 {
				result.Inconclusive = fmt.Sprintf("seek watched file %s: %v", w.path, err)
			}
			found = true
			continue
		}
		data, err := io.ReadAll(f)
		f.Close()
		if err != nil {
			if result.Inconclusive == "" && result.Signal == "" && len(result.Escaped) == 0 {
				result.Inconclusive = fmt.Sprintf("read watched file %s: %v", w.path, err)
			}
			found = true
			continue
		}
		w.offset += int64(len(data))
		w.pending += string(data)
		for {
			j := strings.IndexAny(w.pending, "\r\n")
			if j < 0 {
				break
			}
			line := w.pending[:j]
			w.pending = w.pending[j+1:]
			if t.classifyWatch(inst, line, result, report) {
				found = true
			}
		}
	}
	return found
}
func (t *running) classifyWatch(inst *instance, line string, result *machine.Result, report machine.Reporter) bool {
	switch t.backend.Classify(line).Kind {
	case backend.ComputationError, backend.SetupError, backend.AffinityError:
		return t.classify(inst, line, false, result, report)
	case backend.Other, backend.Progress:
	}
	return false
}

func (t *running) toggle(inst *instance, stop bool, now time.Time) error {
	sig := syscall.SIGCONT
	if stop {
		sig = syscall.SIGSTOP
	}
	if err := t.host.SignalGroup(inst.PID, sig); err != nil && !errors.Is(err, syscall.ESRCH) {
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

func (t *running) handleEvent(e streamEvent, result *machine.Result, report machine.Reporter, unexpected bool) bool {
	inst := t.instances[e.index]
	if e.exit {
		inst.done = true
		found := t.tail(inst, result, report)
		if unexpected && !inst.setup && len(result.Escaped) == 0 && result.Signal == "" {
			result.Signal = machine.UnexpectedExit
			result.Core = inst.Core
			result.Inconclusive = ""
			status := "exit status 0"
			if e.err != nil {
				status = e.err.Error()
			}
			report.Progress(fmt.Sprintf("core %02d backend exited early: %s", inst.Core, status))
			return true
		}
		return found
	}
	if !e.eof && e.err == nil {
		return t.classify(inst, e.line, e.stderr, result, report)
	}
	return false
}

func (t *running) drainEvents(result *machine.Result, report machine.Reporter, unexpected bool) {
	for {
		select {
		case e := <-t.events:
			t.handleEvent(e, result, report, unexpected)
		default:
			return
		}
	}
}

func (t *running) teardown(result *machine.Result, report machine.Reporter) error {
	for _, inst := range t.instances {
		_ = t.host.SignalGroup(inst.PID, syscall.SIGCONT)
		_ = t.host.SignalGroup(inst.PID, syscall.SIGTERM)
	}
	var cleanupErr error
	t.collect(t.options.StopGrace, result, report)
	for _, inst := range t.instances {
		if !t.options.NoScope {
			out, err := t.host.KillScope(inst.Scope)
			if err != nil && !strings.Contains(string(out), "not loaded") && !strings.Contains(string(out), "could not be found") && cleanupErr == nil {
				cleanupErr = fmt.Errorf("kill scope %s: %w: %s", inst.Scope, err, strings.TrimSpace(string(out)))
			}
		}
		if !inst.done {
			_ = t.host.SignalGroup(inst.PID, syscall.SIGKILL)
		}
	}
	if !t.collect(10*time.Second, result, report) {
		for _, inst := range t.instances {
			if !inst.done {
				return fmt.Errorf("backend on core %02d did not exit after SIGKILL", inst.Core)
			}
		}
	}
	for _, inst := range t.instances {
		t.tail(inst, result, report)
		for i := range inst.watch {
			w := &inst.watch[i]
			if w.pending != "" {
				t.classifyWatch(inst, w.pending, result, report)
				w.pending = ""
			}
		}
	}
	return cleanupErr
}
func (t *running) collect(timeout time.Duration, result *machine.Result, report machine.Reporter) bool {
	remaining := 0
	for _, inst := range t.instances {
		if !inst.done {
			remaining++
		}
	}
	timer := time.NewTimer(timeout)
	defer timer.Stop()
	for remaining > 0 {
		select {
		case e := <-t.events:
			if e.exit && !t.instances[e.index].done {
				remaining--
			}
			t.handleEvent(e, result, report, false)
		case <-timer.C:
			return false
		}
	}
	return true
}
func (t *running) abort() {
	for _, inst := range t.instances {
		_ = t.host.SignalGroup(inst.PID, syscall.SIGCONT)
		_ = t.host.SignalGroup(inst.PID, syscall.SIGKILL)
	}
	for _, inst := range t.instances {
		for !inst.done {
			e := <-t.events
			if e.exit {
				t.instances[e.index].done = true
			}
		}
	}
}

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
		report.Sample(machine.Sample{Warning: fmt.Sprintf("stalled: %.1fs cpu in %.1fs running", used.Seconds(), span.Seconds()), PID: inst.PID, TID: inst.PID, CPU: reading.CPU})
		return true
	}
	return false
}
func stalled(a, b cpuSample, threads int) bool {
	span := b.active - a.active
	return span > 0 && b.cpu-a.cpu < time.Duration(float64(span)*0.5*float64(threads))
}

func readTctl(root string) *int {
	dirs, err := os.ReadDir(root)
	if err != nil {
		return nil
	}
	for _, d := range dirs {
		dir := filepath.Join(root, d.Name())
		name, err := os.ReadFile(filepath.Join(dir, "name"))
		if err != nil || (strings.TrimSpace(string(name)) != "k10temp" && strings.TrimSpace(string(name)) != "zenpower") {
			continue
		}
		labels, err := filepath.Glob(filepath.Join(dir, "temp*_label"))
		if err != nil {
			return nil
		}
		for _, label := range labels {
			b, err := os.ReadFile(label)
			if err != nil || strings.TrimSpace(string(b)) != "Tctl" {
				continue
			}
			input := strings.TrimSuffix(label, "_label") + "_input"
			b, err = os.ReadFile(input)
			if err != nil {
				return nil
			}
			value, err := strconv.Atoi(strings.TrimSpace(string(b)))
			if err != nil {
				return nil
			}
			degrees := value / 1000
			return &degrees
		}
		return nil
	}
	return nil
}
