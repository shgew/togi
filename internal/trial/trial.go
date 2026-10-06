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
	User                                               Identity
	NoScope                                            bool
	SampleInterval, StallGrace, StallWindow, StopGrace time.Duration
	Hwmon, CPUFreq, Powercap                           string
	PMTable                                            machine.PMTableReader
	teardown                                           time.Duration
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
	if o.teardown <= 0 {
		o.teardown = teardownLimit
	}
	if o.Hwmon == "" {
		o.Hwmon = "/sys/class/hwmon"
	}
	if o.CPUFreq == "" {
		o.CPUFreq = "/sys/devices/system/cpu"
	}
	if o.Powercap == "" {
		o.Powercap = "/sys/class/powercap"
	}
	return &Runner{options: o, host: newOSHost()}
}

type instance struct {
	machine.Instance
	watch          []watchFile
	watchNext      int
	process        process
	partial        [2]string
	reaped         chan struct{}
	joined         chan struct{}
	done           bool
	writersStopped bool
	unexpected     bool
	exitErr        error
	ready          bool
	setup          bool
	suspended      bool
	resumed        time.Time
	active         time.Duration
	samples        []cpuSample
}

type running struct {
	spec         machine.TrialSpec
	backend      backend.Backend
	options      Options
	host         processHost
	openSamples  func(string) (sampleFile, error)
	started      machine.Started
	instances    []*instance
	scopes       []string
	streamStop   chan struct{}
	streams      sync.WaitGroup
	outputErr    error
	outputCapErr error
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
	if !r.options.NoScope {
		if err := r.options.User.validate(); err != nil {
			return nil, fmt.Errorf("start trial %s: backend_user: %w", spec.ID, err)
		}
	}
	root, err := filepath.Abs(filepath.Join(r.options.Dir, spec.ID))
	if err != nil {
		return nil, fmt.Errorf("resolve trial directory: %w", err)
	}
	if err := os.RemoveAll(root); err != nil {
		return nil, fmt.Errorf("remove trial directory %s: %w", root, err)
	}
	if err := os.MkdirAll(root, 0755); err != nil {
		return nil, fmt.Errorf("create trial directory %s: %w", root, err)
	}
	if !r.options.NoScope {
		for _, dir := range []string{filepath.Dir(root), root} {
			if err := os.Chmod(dir, 0711); err != nil {
				return nil, fmt.Errorf("set backend directory traversal %s: %w", dir, err)
			}
		}
		if err := checkTraversal(root, r.options.User); err != nil {
			return nil, err
		}
	}
	t := &running{spec: spec, backend: b, options: r.options, host: r.host, events: make(chan streamEvent, 1024), streamStop: make(chan struct{})}
	t.openSamples = openSamples
	t.started.Scope = "togi-trial-" + spec.ID
	t.started.CPUs = slices.Clone(spec.CPUs)
	t.started.Schedule = machine.ScheduleFor(spec)
	cores := spec.Cores
	if !spec.Regime.InstancePerCore() {
		cores = []int{spec.Cores[0]}
	}
	ctx, t.cancel = context.WithTimeout(ctx, spec.Duration+time.Duration(len(cores))*30*time.Second+r.options.StopGrace+20*time.Second)
	for i, core := range cores {
		cpus := spec.CPUs
		prefix := "work"
		dir := filepath.Join(root, prefix)
		scope := t.started.Scope
		if spec.Regime.InstancePerCore() {
			cpus = []int{spec.CPUs[i]}
			prefix = fmt.Sprintf("c%02d", core)
			dir = filepath.Join(root, prefix)
			scope += "-" + prefix
		}
		prepared, err := t.prepareInstance(core, dir, cpus, prefix)
		if err != nil {
			return nil, t.abort(err)
		}
		argv := prepared.launch.Argv
		if !r.options.NoScope {
			argv = scopeArgv(scope, cpus, r.options.User, dir, argv...)
		}
		if !r.options.NoScope {
			t.scopes = append(t.scopes, scope)
		}
		p, err := t.host.Start(ctx, argv, dir)
		if err != nil {
			prepared.closeLogs()
			return nil, t.abort(fmt.Errorf("start %s: %w", scope, err))
		}
		inst := &instance{Core: core, CPUs: slices.Clone(cpus), PID: p.PID(), Scope: scope, process: p, resumed: time.Now(), reaped: make(chan struct{}), joined: make(chan struct{})}
		for _, name := range prepared.launch.Watch {
			inst.watch = append(inst.watch, watchFile{path: filepath.Join(dir, name)})
		}
		t.instances = append(t.instances, inst)
		t.started.Instances = append(t.started.Instances, inst.Instance)
		if i == 0 {
			t.started.PID = p.PID()
			t.started.Argv = slices.Clone(prepared.launch.Argv)
		}
		done := make(chan struct{}, 2)
		t.streams.Add(2)
		go t.readStream(i, inst, p.Stdout(), prepared.stdout, false, done)
		go t.readStream(i, inst, p.Stderr(), prepared.stderr, true, done)
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

type preparedInstance struct {
	launch         backend.Launch
	stdout, stderr *os.File
}

func (p preparedInstance) closeLogs() {
	p.stdout.Close()
	p.stderr.Close()
}

func (t *running) prepareInstance(core int, dir string, cpus []int, prefix string) (preparedInstance, error) {
	if err := os.MkdirAll(dir, 0755); err != nil {
		return preparedInstance{}, fmt.Errorf("create instance directory %s: %w", dir, err)
	}
	if err := os.Chmod(dir, 0755); err != nil {
		return preparedInstance{}, fmt.Errorf("set instance directory permissions %s: %w", dir, err)
	}
	launch, err := t.backend.Prepare(t.spec.Workload, dir, cpus)
	if err != nil {
		return preparedInstance{}, fmt.Errorf("prepare %s on core %02d: %w", t.backend.Name(), core, err)
	}
	if len(launch.Argv) == 0 {
		return preparedInstance{}, fmt.Errorf("prepare %s on core %02d: empty argv", t.backend.Name(), core)
	}
	out, err := os.OpenFile(filepath.Join(dir, "stdout.log"), os.O_CREATE|os.O_WRONLY|os.O_EXCL, 0644)
	if err != nil {
		return preparedInstance{}, fmt.Errorf("open stdout log: %w", err)
	}
	erlog, err := os.OpenFile(filepath.Join(dir, "stderr.log"), os.O_CREATE|os.O_WRONLY|os.O_EXCL, 0644)
	if err != nil {
		out.Close()
		return preparedInstance{}, fmt.Errorf("open stderr log: %w", err)
	}
	prepared := preparedInstance{launch: launch, stdout: out, stderr: erlog}
	if !t.options.NoScope {
		if err := ownDirectory(dir, launch.Files, t.options.User, t.host.Chown); err != nil {
			prepared.closeLogs()
			return preparedInstance{}, err
		}
	}
	for _, f := range launch.Files {
		t.started.Files = append(t.started.Files, filepath.Join(prefix, f))
	}
	return prepared, nil
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
func scopeArgv(unit string, cpus []int, user Identity, dir string, argv ...string) []string {
	a := []string{"systemd-run"}
	if os.Geteuid() != 0 {
		a = append(a, "--user")
	}
	a = append(a, "--scope", "--quiet", "--collect", "--unit", unit, "--uid", strconv.FormatUint(uint64(user.UID), 10), "--gid", strconv.FormatUint(uint64(user.GID), 10), "--working-directory", dir, "-p", "AllowedCPUs="+joinCPUs(cpus), "-p", "DefaultDependencies=no", "--")
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
			used, lineErr := lines.consume(context.Background(), buf[:n], func(line string) {
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
