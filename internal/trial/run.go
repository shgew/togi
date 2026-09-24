package trial

import (
	"context"
	"errors"
	"fmt"
	"io"
	"iter"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"syscall"
	"time"

	"code.marleb.org/shgew/shycler/internal/backend"
	"code.marleb.org/shgew/shycler/internal/machine"
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
				if !inst.ready && ready(inst, t.options.NoScope) {
					inst.ready = true
				}
				if !inst.ready || (inst.suspended && t.options.NoScope) {
					continue
				}
				if cpu, tid, escaped := outsideCPU(inst); escaped {
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
			if t.spec.Regime == machine.R7 {
				if step.At == t.spec.Duration/2 {
					report.Progress("CCD0 only: stopped cores 08-15")
				}
				if step.At == t.spec.Duration*3/4 && !step.Stop {
					report.Progress("CCD1 only: resumed cores 08-15, stopped cores 00-07")
				}
			}
			for _, idx := range step.Instances {
				inst := t.instances[idx]
				if err := toggleInstance(inst, step.Stop, now); err != nil {
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
			if !inst.ready && !ready(inst, false) && result.Signal == "" && len(result.Escaped) == 0 && result.Inconclusive == "" {
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
			report.Progress(fmt.Sprintf("core %02d computation error: %s", inst.Core, line))
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

func toggleInstance(inst *instance, stop bool, now time.Time) error {
	sig := syscall.SIGCONT
	if stop {
		sig = syscall.SIGSTOP
	}
	if err := syscall.Kill(-inst.PID, sig); err != nil && !errors.Is(err, syscall.ESRCH) {
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
			report.Progress(fmt.Sprintf("core %02d backend exited early: %v", inst.Core, e.err))
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
		_ = syscall.Kill(-inst.PID, syscall.SIGCONT)
		_ = syscall.Kill(-inst.PID, syscall.SIGTERM)
	}
	var cleanupErr error
	t.collect(t.options.StopGrace, result, report)
	for _, inst := range t.instances {
		if !t.options.NoScope {
			args := []string{}
			if os.Geteuid() != 0 {
				args = append(args, "--user")
			}
			args = append(args, "kill", "--signal=SIGKILL", "--kill-whom=all", inst.Scope+".scope")
			out, err := exec.Command("systemctl", args...).CombinedOutput()
			if err != nil && !strings.Contains(string(out), "not loaded") && !strings.Contains(string(out), "could not be found") && cleanupErr == nil {
				cleanupErr = fmt.Errorf("kill scope %s: %w: %s", inst.Scope, err, strings.TrimSpace(string(out)))
			}
		}
		if !inst.done {
			_ = syscall.Kill(-inst.PID, syscall.SIGKILL)
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
		_ = syscall.Kill(-inst.PID, syscall.SIGCONT)
		_ = syscall.Kill(-inst.PID, syscall.SIGKILL)
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

func ready(inst *instance, noScope bool) bool {
	if noScope {
		return true
	}
	b, err := os.ReadFile(fmt.Sprintf("/proc/%d/cgroup", inst.PID))
	if err != nil {
		return false
	}
	for line := range strings.SplitSeq(strings.TrimSpace(string(b)), "\n") {
		if strings.HasSuffix(line, "/"+inst.Scope+".scope") {
			return true
		}
	}
	return false
}

func procStat(path string) (fields []string, err error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	end := strings.LastIndexByte(string(b), ')')
	if end < 0 {
		return nil, fmt.Errorf("malformed proc stat %s", path)
	}
	fields = strings.Fields(string(b[end+1:]))
	if len(fields) < 37 {
		return nil, fmt.Errorf("short proc stat %s", path)
	}
	return fields, nil
}
func fieldInt(fields []string, number int) int64 {
	v, _ := strconv.ParseInt(fields[number-3], 10, 64)
	return v
}
func outsideCPU(inst *instance) (cpu, tid int, escaped bool) {
	tasks, err := os.ReadDir(fmt.Sprintf("/proc/%d/task", inst.PID))
	if err != nil {
		return
	}
	for _, task := range tasks {
		stat, err := procStat(fmt.Sprintf("/proc/%d/task/%s/stat", inst.PID, task.Name()))
		if err != nil {
			continue
		}
		cpu = int(fieldInt(stat, 39))
		if !slices.Contains(inst.CPUs, cpu) {
			tid, _ = strconv.Atoi(task.Name())
			return cpu, tid, true
		}
	}
	return 0, 0, false
}
func (t *running) sample(inst *instance, now time.Time, result *machine.Result, report machine.Reporter) bool {
	fields, err := procStat(fmt.Sprintf("/proc/%d/stat", inst.PID))
	if err != nil {
		return false
	}
	active := inst.active
	if !inst.suspended {
		active += now.Sub(inst.resumed)
	}
	cpu := time.Duration(fieldInt(fields, 14)+fieldInt(fields, 15)) * time.Second / 100
	current := cpuSample{active: active, cpu: cpu}
	inst.samples = append(inst.samples, current)
	if active < t.options.StallGrace {
		return false
	}
	latest := -1
	for i := len(inst.samples) - 1; i >= 0; i-- {
		earlier := inst.samples[i]
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
		report.Sample(machine.Sample{Warning: fmt.Sprintf("stalled: %.1fs cpu in %.1fs running", used.Seconds(), span.Seconds()), PID: inst.PID, TID: inst.PID, CPU: int(fieldInt(fields, 39))})
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
