package hardware

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/shgew/togi/internal/backend"
	"github.com/shgew/togi/internal/backend/mprime"
	"github.com/shgew/togi/internal/backend/ycruncher"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/detect"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/smu"
	"github.com/shgew/togi/internal/trial"
	"golang.org/x/sys/unix"
)

// CheckPlatform reports whether this platform can run on real hardware.
func CheckPlatform() error { return nil }

func New(cfg config.Config, stateDir string) (machine.Machine, error) {
	drv, err := smu.Open("/", smu.Sysfs("/"))
	if err != nil {
		return machine.Machine{}, fmt.Errorf("open SMU: %w", err)
	}
	cores := drv.Topology()
	h := newHost(drv, cfg)
	h.pmTable = smu.NewPMTableReader("/", cores, os.ReadFile)
	return machine.Machine{
		Clock:  clock{},
		SMU:    drv,
		Host:   h,
		Trials: trial.New(trial.Options{Dir: filepath.Join(stateDir, "trials"), Backends: h.backends, Cores: cores, User: h.user, PMTable: h.pmTable}),
		Kernel: detect.NewKernel(cores),
	}, nil
}

func newHost(drv *smu.Driver, cfg config.Config) *host {
	backends := map[machine.Backend]backend.Backend{}
	if cfg.Backends.Mprime != "" {
		backends[machine.Mprime] = mprime.New(cfg.Backends.Mprime)
	}
	if cfg.Backends.Ycruncher != "" {
		backends[machine.Ycruncher] = ycruncher.New(cfg.Backends.Ycruncher)
	}
	user, userErr := trial.LookupIdentity(cfg.BackendUser)
	return &host{drv: drv, cfg: cfg, backends: backends, user: user, userErr: userErr}
}

type clock struct{}

func (clock) Now() time.Time { return time.Now() }

func (clock) Monotonic() time.Duration {
	var ts unix.Timespec
	if err := unix.ClockGettime(unix.CLOCK_MONOTONIC, &ts); err != nil {
		panic(fmt.Errorf("read monotonic clock: %w", err))
	}
	return time.Duration(ts.Nano())
}

func (clock) Sleep(ctx context.Context, d time.Duration) error {
	timer := time.NewTimer(d)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-timer.C:
		return nil
	}
}

type host struct {
	drv      *smu.Driver
	cfg      config.Config
	backends map[machine.Backend]backend.Backend
	user     trial.Identity
	userErr  error
	pmTable  *smu.PMTableReader
}

func (h *host) BootID() (string, error) { return detect.BootID() }

func (h *host) Topology() ([]machine.CoreInfo, error) { return h.drv.Topology(), nil }

func (h *host) ValidateSMU() error { return h.drv.ValidateSMU() }

func (h *host) BIOSContext() (machine.BIOSContext, error) { return h.drv.BIOSContext() }

func (h *host) Ranking() ([]machine.CoreRank, error) { return ranking("/", h.drv.Topology()) }

func ranking(root string, cores []machine.CoreInfo) ([]machine.CoreRank, error) {
	values := make([]machine.CoreRank, len(cores))
	for i, core := range cores {
		path := filepath.Join(root, "sys/devices/system/cpu/cpufreq", fmt.Sprintf("policy%d", core.CPUs[0]), "amd_pstate_prefcore_ranking")
		raw, err := os.ReadFile(path)
		if err != nil {
			return nil, fmt.Errorf("read preferred-core ranking of cpu %d: %w", core.CPUs[0], err)
		}
		value, err := strconv.Atoi(strings.TrimSpace(string(raw)))
		if err != nil {
			return nil, fmt.Errorf("read preferred-core ranking of cpu %d: %w", core.CPUs[0], err)
		}
		values[i] = machine.CoreRank{Core: core.Core, Value: value}
	}
	return values, nil
}

// preflightCheck is one check of run's preflight. The checks after a failed
// identity check (cpu, ryzen_smu) need that identity, so they do not run.
// Privileged checks need root: the SMU mailbox, the PM table or systemd.
type preflightCheck struct {
	name       string
	identity   bool
	gated      bool
	privileged bool
	run        func(*host) machine.Check
}

var preflightChecks = []preflightCheck{
	{name: "cpu", identity: true, run: func(h *host) machine.Check { return h.drv.CheckCPU() }},
	{name: "ryzen_smu", identity: true, run: func(h *host) machine.Check { return h.drv.CheckDriver() }},
	{name: "pm_table", privileged: true, run: func(h *host) machine.Check { return h.pmTable.Check() }},
	{name: "readback", gated: true, privileged: true, run: func(h *host) machine.Check { return h.drv.CheckReadback() }},
	{name: "slot_mapping", gated: true, privileged: true, run: func(h *host) machine.Check { return h.drv.CheckSlotMapping() }},
	{name: "backends", gated: true, run: (*host).checkBackends},
	{name: "backend_user", gated: true, run: (*host).checkBackendUser},
	{name: "systemd_run", gated: true, privileged: true, run: (*host).checkSystemdRun},
}

const needsRoot = "needs root"

func (h *host) Preflight() []machine.Check {
	root := machine.Check{Name: "root", Detail: "uid 0", OK: true}
	if uid := os.Geteuid(); uid != 0 {
		root = machine.Check{Name: "root", Detail: fmt.Sprintf("running as uid %d; run needs root", uid)}
	}
	checks, _ := h.preflight(true)
	return append([]machine.Check{root}, checks...)
}

// preflight runs the checks in order; unprivileged, it skips those that need root.
func (h *host) preflight(privileged bool) (ran, skipped []machine.Check) {
	identified := true
	for _, c := range preflightChecks {
		switch {
		case c.privileged && !privileged:
			skipped = append(skipped, machine.Check{Name: c.name, Detail: needsRoot})
		case c.gated && !identified:
			skipped = append(skipped, machine.Check{Name: c.name, Detail: "needs passing cpu and ryzen_smu checks"})
		default:
			check := c.run(h)
			ran = append(ran, check)
			if c.identity && !check.OK {
				identified = false
			}
		}
	}
	return ran, skipped
}

// Diagnose runs the checks run's preflight makes, then the watchdog check and,
// with a recorded BIOS context, the bios_context comparison. It writes
// nothing. Unprivileged, it opens the SMU driver without SMN reads and
// returns the checks that need root as skipped, each Detail saying why.
func Diagnose(cfg config.Config, recorded *machine.BIOSContext, privileged bool) (ran, skipped []machine.Check, err error) {
	return diagnose("/", smu.Sysfs("/"), cfg, recorded, privileged)
}

func diagnose(root string, mb smu.Mailbox, cfg config.Config, recorded *machine.BIOSContext, privileged bool) (ran, skipped []machine.Check, err error) {
	open := smu.OpenIdentity
	if privileged {
		open = smu.Open
	}
	drv, err := open(root, mb)
	if err != nil {
		return nil, nil, fmt.Errorf("open SMU: %w", err)
	}
	h := newHost(drv, cfg)
	if privileged {
		h.pmTable = smu.NewPMTableReader(root, drv.Topology(), os.ReadFile)
	}
	ran, skipped = h.preflight(privileged)
	passed := !slices.ContainsFunc(ran, func(c machine.Check) bool { return !c.OK })
	ran = append(ran, watchdog(root))
	if recorded == nil {
		return ran, skipped, nil
	}
	switch {
	case !privileged:
		skipped = append(skipped, machine.Check{Name: "bios_context", Detail: needsRoot})
	case !passed:
		// Like run, compare only after every other check passed: a failed one may leave the SMU unreachable.
		skipped = append(skipped, machine.Check{Name: "bios_context", Detail: "needs every other check to pass"})
	default:
		check := machine.Check{Name: "bios_context"}
		if current, err := drv.BIOSContext(); err != nil {
			check.Detail = err.Error()
		} else {
			check.Detail, check.OK = machine.CompareContext(*recorded, current)
		}
		ran = append(ran, check)
	}
	return ran, skipped, nil
}

func (h *host) checkSystemdRun() machine.Check {
	c := machine.Check{Name: "systemd_run", OK: true}
	detail, err := trial.CheckSystemdRun(h.user)
	c.Detail = detail
	if err != nil {
		c.Detail, c.OK = err.Error(), false
	}
	return c
}

func (h *host) Watchdog() machine.Check { return watchdog("/") }

func (h *host) checkBackendUser() machine.Check {
	c := machine.Check{Name: "backend_user", OK: h.userErr == nil}
	if h.userErr != nil {
		c.Detail = h.userErr.Error()
	} else {
		c.Detail = fmt.Sprintf("%s: uid %d gid %d", h.cfg.BackendUser, h.user.UID, h.user.GID)
	}
	return c
}

func (h *host) checkBackends() machine.Check {
	c := machine.Check{Name: "backends", OK: true}
	var details []string
	for _, b := range []struct {
		kind machine.Backend
		key  string
	}{{machine.Mprime, "mprime"}, {machine.Ycruncher, "ycruncher"}} {
		be, ok := h.backends[b.kind]
		if !ok {
			details, c.OK = append(details, fmt.Sprintf("backends.%s not configured", b.key)), false
			continue
		}
		detail, err := be.Check()
		if err != nil {
			details, c.OK = append(details, fmt.Sprintf("%s: %v", be.Name(), err)), false
			continue
		}
		details = append(details, fmt.Sprintf("%s: %s", be.Name(), detail))
	}
	c.Detail = strings.Join(details, "; ")
	return c
}
