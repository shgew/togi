package trial

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/shgew/togi/internal/backend"
	"github.com/shgew/togi/internal/machine"
)

type Options struct {
	Dir                                                string
	Backends                                           map[machine.Backend]backend.Backend
	Cores                                              []machine.CoreInfo
	NoScope                                            bool
	SampleInterval, StallGrace, StallWindow, StopGrace time.Duration
	Hwmon                                              string
}

type Runner struct {
	options Options
	host    processHost
}

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
	if o.StopGrace <= 0 || o.StopGrace > 3*time.Second {
		o.StopGrace = 3 * time.Second
	}
	if o.Hwmon == "" {
		o.Hwmon = "/sys/class/hwmon"
	}
	return &Runner{options: o, host: newOSHost()}
}

type instance struct {
	machine.Instance
	watch     []watchFile
	process   process
	partial   [2]string
	reaped    chan struct{}
	joined    chan struct{}
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
	host         processHost
	started      machine.Started
	instances    []*instance
	scopes       []string
	streamStop   chan struct{}
	streams      sync.WaitGroup
	outputErr    error
	stopMu       sync.Mutex
	stopped      bool
	streamClosed bool
	cleanupErr   error
	events       chan streamEvent
	initialStops int
	cancel       context.CancelFunc
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
	t := &running{spec: spec, backend: b, options: r.options, host: r.host, events: make(chan streamEvent, 1024), streamStop: make(chan struct{})}
	t.started.Scope = "togi-trial-" + spec.ID
	t.started.CPUs = slices.Clone(spec.CPUs)
	t.started.Schedule = machine.ScheduleFor(spec)
	cores := spec.Cores
	if !spec.Regime.AllCores() {
		cores = []int{spec.Cores[0]}
	}
	ctx, t.cancel = context.WithTimeout(ctx, spec.Duration+time.Duration(len(cores))*30*time.Second+r.options.StopGrace+20*time.Second)
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
			return nil, t.abort(fmt.Errorf("create instance directory %s: %w", dir, err))
		}
		launch, err := b.Prepare(spec.Workload, dir, cpus)
		if err != nil {
			return nil, t.abort(fmt.Errorf("prepare %s on core %02d: %w", b.Name(), core, err))
		}
		for _, f := range launch.Files {
			if prefix != "" {
				f = filepath.Join(prefix, f)
			}
			t.started.Files = append(t.started.Files, f)
		}
		if len(launch.Argv) == 0 {
			return nil, t.abort(fmt.Errorf("prepare %s on core %02d: empty argv", b.Name(), core))
		}
		argv := launch.Argv
		if !r.options.NoScope {
			argv = scopeArgv(scope, cpus, launch.Argv...)
		}
		out, err := os.OpenFile(filepath.Join(dir, "stdout.log"), os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0644)
		if err != nil {
			return nil, t.abort(fmt.Errorf("open stdout log: %w", err))
		}
		erlog, err := os.OpenFile(filepath.Join(dir, "stderr.log"), os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0644)
		if err != nil {
			out.Close()
			return nil, t.abort(fmt.Errorf("open stderr log: %w", err))
		}
		if !r.options.NoScope {
			t.scopes = append(t.scopes, scope)
		}
		p, err := t.host.Start(ctx, argv, dir)
		if err != nil {
			out.Close()
			erlog.Close()
			return nil, t.abort(fmt.Errorf("start %s: %w", scope, err))
		}
		inst := &instance{Core: core, CPUs: slices.Clone(cpus), PID: p.PID(), Scope: scope, process: p, resumed: time.Now(), reaped: make(chan struct{}), joined: make(chan struct{})}
		for _, name := range launch.Watch {
			inst.watch = append(inst.watch, watchFile{path: filepath.Join(dir, name)})
		}
		t.instances = append(t.instances, inst)
		t.started.Instances = append(t.started.Instances, inst.Instance)
		if i == 0 {
			t.started.PID = p.PID()
			t.started.Argv = slices.Clone(launch.Argv)
		}
		done := make(chan struct{}, 2)
		t.streams.Add(2)
		go t.readStream(i, inst, p.Stdout(), out, false, done)
		go t.readStream(i, inst, p.Stderr(), erlog, true, done)
		go func(index int, p process) {
			defer close(inst.joined)
			err := p.Wait()
			close(inst.reaped)
			<-done
			<-done
			t.emit(streamEvent{index: index, exit: true, err: err})
		}(i, p)
		if spec.Regime == machine.R6 {
			if !r.options.NoScope {
				t.awaitScope(ctx, inst)
			}
			if err := t.toggle(inst, true, time.Now()); err != nil {
				return nil, t.abort(fmt.Errorf("stop initial R6 instance on core %02d: %w", core, err))
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
		if t.inScope(inst) {
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

// scopeArgv wraps argv in a transient scope confined to cpus. DefaultDependencies=no keeps a system shutdown from
// stopping the scope before togi: togi's own teardown ends the trial, so it ends interrupted, not failed.
func scopeArgv(unit string, cpus []int, argv ...string) []string {
	a := []string{"systemd-run"}
	if os.Geteuid() != 0 {
		a = append(a, "--user")
	}
	a = append(a, "--scope", "--quiet", "--collect", "--unit", unit, "-p", "AllowedCPUs="+joinCPUs(cpus), "-p", "DefaultDependencies=no", "--")
	return append(a, argv...)
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

func (t *running) emit(e streamEvent) {
	select {
	case t.events <- e:
	case <-t.streamStop:
	}
}

func (t *running) readStream(i int, inst *instance, src io.Reader, log *os.File, stderr bool, done chan<- struct{}) {
	var lines outputLines
	defer func() {
		stream := 0
		if stderr {
			stream = 1
		}
		if !lines.exceeded {
			inst.partial[stream] = string(lines.pending)
		}
		log.Close()
		done <- struct{}{}
		t.streams.Done()
	}()
	buf := make([]byte, 4096)
	for {
		n, err := src.Read(buf)
		if n > 0 {
			used, lineErr := lines.consume(buf[:n], func(line string) {
				t.emit(streamEvent{index: i, line: line, stderr: stderr})
			})
			if _, werr := log.Write(buf[:used]); werr != nil {
				t.emit(streamEvent{index: i, err: fmt.Errorf("write output log: %w", werr)})
			}
			if lineErr != nil {
				t.emit(streamEvent{index: i, err: fmt.Errorf("read %s: %w", filepath.Base(log.Name()), lineErr)})
				return
			}
		}
		select {
		case <-t.streamStop:
			return
		default:
		}
		if err != nil {
			if !errors.Is(err, io.EOF) {
				t.emit(streamEvent{index: i, err: errors.Join(machine.ErrContainment, fmt.Errorf("read backend output: %w", err))})
			}
			t.emit(streamEvent{index: i, stderr: stderr, eof: true})
			return
		}
	}
}
