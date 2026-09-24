package trial

import (
	"context"
	"fmt"
	"io"
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

type Options struct {
	Dir                                                string
	Backends                                           map[machine.Backend]backend.Backend
	Cores                                              []machine.CoreInfo
	NoScope                                            bool
	SampleInterval, StallGrace, StallWindow, StopGrace time.Duration
	Hwmon                                              string
}

type Runner struct{ options Options }

func New(o Options) *Runner {
	if o.SampleInterval <= 0 {
		o.SampleInterval = time.Second
	}
	if o.StallGrace <= 0 {
		o.StallGrace = 30 * time.Second
	}
	if o.StallWindow <= 0 {
		o.StallWindow = 10 * time.Second
	}
	if o.StopGrace <= 0 {
		o.StopGrace = 3 * time.Second
	}
	if o.Hwmon == "" {
		o.Hwmon = "/sys/class/hwmon"
	}
	return &Runner{options: o}
}

type instance struct {
	machine.Instance
	watch     []watchFile
	done      bool
	ready     bool
	setup     bool
	suspended bool
	resumed   time.Time
	active    time.Duration
	samples   []cpuSample
}

type running struct {
	spec         machine.TrialSpec
	backend      backend.Backend
	options      Options
	started      machine.Started
	instances    []*instance
	events       chan streamEvent
	initialStops int
}

type streamEvent struct {
	index  int
	line   string
	stderr bool
	eof    bool
	exit   bool
	err    error
}

func (r *Runner) Start(ctx context.Context, spec machine.TrialSpec) (machine.Running, error) {
	b := r.options.Backends[spec.Workload.Backend]
	if b == nil {
		return nil, fmt.Errorf("start trial %s: backend %s not configured", spec.ID, spec.Workload.Backend)
	}
	root := filepath.Join(r.options.Dir, spec.ID)
	if err := os.RemoveAll(root); err != nil {
		return nil, fmt.Errorf("remove trial directory %s: %w", root, err)
	}
	if err := os.MkdirAll(root, 0755); err != nil {
		return nil, fmt.Errorf("create trial directory %s: %w", root, err)
	}
	t := &running{spec: spec, backend: b, options: r.options, events: make(chan streamEvent, 1024)}
	t.started.Scope = "shycler-trial-" + spec.ID
	t.started.CPUs = slices.Clone(spec.CPUs)
	t.started.Schedule = machine.ScheduleFor(spec)
	cores := spec.Cores
	if !spec.Regime.AllCores() {
		cores = []int{spec.Cores[0]}
	}
	for i, core := range cores {
		cpus := spec.CPUs
		dir := root
		scope := t.started.Scope
		prefix := ""
		if spec.Regime.AllCores() {
			cpus = []int{spec.CPUs[i]}
			prefix = fmt.Sprintf("c%02d", core)
			dir = filepath.Join(root, prefix)
			scope += "-" + prefix
		}
		if err := os.MkdirAll(dir, 0755); err != nil {
			t.abort()
			return nil, fmt.Errorf("create instance directory %s: %w", dir, err)
		}
		launch, err := b.Prepare(spec.Workload, dir, cpus)
		if err != nil {
			t.abort()
			return nil, fmt.Errorf("prepare %s on core %02d: %w", b.Name(), core, err)
		}
		for _, f := range launch.Files {
			if prefix != "" {
				f = filepath.Join(prefix, f)
			}
			t.started.Files = append(t.started.Files, f)
		}
		if len(launch.Argv) == 0 {
			t.abort()
			return nil, fmt.Errorf("prepare %s on core %02d: empty argv", b.Name(), core)
		}
		argv := launch.Argv
		if !r.options.NoScope {
			argv = []string{"systemd-run"}
			if os.Geteuid() != 0 {
				argv = append(argv, "--user")
			}
			argv = append(argv, "--scope", "--quiet", "--collect", "--unit", scope, "-p", "AllowedCPUs="+joinCPUs(cpus), "--")
			argv = append(argv, launch.Argv...)
		}
		cmd := exec.CommandContext(ctx, argv[0], argv[1:]...)
		cmd.Dir = dir
		cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true, Pdeathsig: syscall.SIGKILL}
		in, err := os.Open(os.DevNull)
		if err != nil {
			t.abort()
			return nil, fmt.Errorf("open stdin: %w", err)
		}
		out, err := os.OpenFile(filepath.Join(dir, "stdout.log"), os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0644)
		if err != nil {
			in.Close()
			t.abort()
			return nil, fmt.Errorf("open stdout log: %w", err)
		}
		erlog, err := os.OpenFile(filepath.Join(dir, "stderr.log"), os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0644)
		if err != nil {
			in.Close()
			out.Close()
			t.abort()
			return nil, fmt.Errorf("open stderr log: %w", err)
		}
		stdout, err := cmd.StdoutPipe()
		if err != nil {
			in.Close()
			out.Close()
			erlog.Close()
			t.abort()
			return nil, fmt.Errorf("pipe stdout: %w", err)
		}
		stderr, err := cmd.StderrPipe()
		if err != nil {
			in.Close()
			out.Close()
			erlog.Close()
			t.abort()
			return nil, fmt.Errorf("pipe stderr: %w", err)
		}
		cmd.Stdin = in
		if err := cmd.Start(); err != nil {
			in.Close()
			out.Close()
			erlog.Close()
			t.abort()
			return nil, fmt.Errorf("start %s: %w", scope, err)
		}
		in.Close()
		inst := &instance{Instance: machine.Instance{Core: core, CPUs: slices.Clone(cpus), PID: cmd.Process.Pid, Scope: scope}, resumed: time.Now()}
		for _, name := range launch.Watch {
			inst.watch = append(inst.watch, watchFile{path: filepath.Join(dir, name)})
		}
		t.instances = append(t.instances, inst)
		t.started.Instances = append(t.started.Instances, inst.Instance)
		if i == 0 {
			t.started.PID = cmd.Process.Pid
			t.started.Argv = slices.Clone(launch.Argv)
		}
		done := make(chan struct{}, 2)
		go t.readStream(i, stdout, out, false, done)
		go t.readStream(i, stderr, erlog, true, done)
		go func(index int, c *exec.Cmd) {
			<-done
			<-done
			t.events <- streamEvent{index: index, exit: true, err: c.Wait()}
		}(i, cmd)
		if spec.Regime == machine.R6 {
			if !r.options.NoScope {
				t.awaitScope(ctx, inst)
			}
			if err := toggleInstance(inst, true, time.Now()); err != nil {
				t.abort()
				return nil, fmt.Errorf("stop initial R6 instance on core %02d: %w", core, err)
			}
			inst.active = 0
			t.initialStops++
		}
	}
	return t, nil
}

func (t *running) awaitScope(ctx context.Context, inst *instance) {
	timer := time.NewTimer(min(t.options.StallGrace, 30*time.Second))
	defer timer.Stop()
	ticker := time.NewTicker(10 * time.Millisecond)
	defer ticker.Stop()
	for {
		if ready(inst, false) {
			inst.ready = true
			return
		}
		select {
		case <-ctx.Done():
			return
		case <-timer.C:
			return
		case <-ticker.C:
		}
	}
}

func joinCPUs(cpus []int) string {
	a := make([]string, len(cpus))
	for i, c := range cpus {
		a[i] = strconv.Itoa(c)
	}
	return strings.Join(a, ",")
}

func CheckSystemdRun() (string, error) {
	argv := []string{}
	if os.Geteuid() != 0 {
		argv = append(argv, "--user")
	}
	argv = append(argv, "--scope", "--quiet", "--collect", "--unit", fmt.Sprintf("shycler-preflight-%d", os.Getpid()), "-p", "AllowedCPUs=0", "--", "/bin/sh", "-c", "exit 0")
	out, err := exec.Command("systemd-run", argv...).CombinedOutput()
	if err != nil {
		return "", fmt.Errorf("systemd-run: %w: %s", err, strings.TrimSpace(string(out)))
	}
	return "scope confined to cpu 0 created", nil
}

func (r *Runner) Passed(id string) error {
	dir := filepath.Join(r.options.Dir, id)
	if err := os.WriteFile(filepath.Join(dir, "passed"), nil, 0644); err != nil {
		return fmt.Errorf("mark trial %s passed: %w", id, err)
	}
	entries, err := os.ReadDir(r.options.Dir)
	if err != nil {
		return fmt.Errorf("list passed trials: %w", err)
	}
	var passed []string
	for _, e := range entries {
		if e.IsDir() {
			if _, err := os.Stat(filepath.Join(r.options.Dir, e.Name(), "passed")); err == nil {
				passed = append(passed, e.Name())
			} else if !os.IsNotExist(err) {
				return fmt.Errorf("stat passed marker %s: %w", e.Name(), err)
			}
		}
	}
	slices.SortFunc(passed, func(a, b string) int {
		ai, ae := strconv.ParseUint(a, 10, 64)
		bi, be := strconv.ParseUint(b, 10, 64)
		if ae == nil && be == nil {
			if ai < bi {
				return -1
			}
			if ai > bi {
				return 1
			}
			return 0
		}
		return strings.Compare(a, b)
	})
	for _, id := range passed[:max(0, len(passed)-200)] {
		if err := os.RemoveAll(filepath.Join(r.options.Dir, id)); err != nil {
			return fmt.Errorf("prune passed trial %s: %w", id, err)
		}
	}
	return nil
}

func (t *running) Started() machine.Started { return t.started }

func (t *running) readStream(i int, src io.Reader, log *os.File, stderr bool, done chan<- struct{}) {
	defer func() { log.Close(); done <- struct{}{} }()
	buf := make([]byte, 4096)
	var pending []byte
	for {
		n, err := src.Read(buf)
		if n > 0 {
			if _, werr := log.Write(buf[:n]); werr != nil {
				t.events <- streamEvent{index: i, err: fmt.Errorf("write output log: %w", werr)}
			}
			pending = append(pending, buf[:n]...)
			for {
				j := bytesIndexDelimiter(pending)
				if j < 0 {
					break
				}
				t.events <- streamEvent{index: i, line: string(pending[:j]), stderr: stderr}
				pending = pending[j+1:]
			}
		}
		if err != nil {
			if len(pending) > 0 {
				t.events <- streamEvent{index: i, line: string(pending), stderr: stderr}
			}
			if err != io.EOF {
				t.events <- streamEvent{index: i, err: fmt.Errorf("read backend output: %w", err)}
			}
			t.events <- streamEvent{index: i, stderr: stderr, eof: true}
			return
		}
	}
}
func bytesIndexDelimiter(b []byte) int {
	for i, c := range b {
		if c == '\n' || c == '\r' {
			return i
		}
	}
	return -1
}
