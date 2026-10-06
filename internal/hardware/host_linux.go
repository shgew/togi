package hardware

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
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
	backends := map[machine.Backend]backend.Backend{}
	if cfg.Backends.Mprime != "" {
		backends[machine.Mprime] = mprime.New(cfg.Backends.Mprime)
	}
	if cfg.Backends.Ycruncher != "" {
		backends[machine.Ycruncher] = ycruncher.New(cfg.Backends.Ycruncher)
	}
	cores := drv.Topology()
	pmTable := smu.NewPMTableReader("/", cores, os.ReadFile)
	user, userErr := trial.LookupIdentity(cfg.BackendUser)
	h := &host{drv: drv, cfg: cfg, backends: backends, user: user, userErr: userErr, pmTable: pmTable}
	return machine.Machine{
		Clock:  clock{},
		SMU:    drv,
		Host:   h,
		Trials: trial.New(trial.Options{Dir: filepath.Join(stateDir, "trials"), Backends: backends, Cores: cores, User: user, PMTable: pmTable}),
		Kernel: detect.NewKernel(cores),
	}, nil
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

func (h *host) Ranking() ([]int, error) { return ranking("/", h.drv.Topology()) }

func ranking(root string, cores []machine.CoreInfo) ([]int, error) {
	values := make([]int, len(cores))
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
		values[i] = value
	}
	return values, nil
}

func (h *host) Preflight() []machine.Check {
	root := machine.Check{Name: "root", Detail: "uid 0", OK: true}
	if uid := os.Geteuid(); uid != 0 {
		root = machine.Check{Name: "root", Detail: fmt.Sprintf("running as uid %d; run needs root", uid)}
	}
	identity := []machine.Check{root, h.drv.CheckCPU(), h.drv.CheckDriver(), h.pmTable.Check()}
	if !identity[1].OK || !identity[2].OK {
		return identity
	}
	systemdRun := machine.Check{Name: "systemd_run", OK: true}
	detail, err := trial.CheckSystemdRun(h.user)
	systemdRun.Detail = detail
	if err != nil {
		systemdRun.Detail, systemdRun.OK = err.Error(), false
	}
	return []machine.Check{
		root,
		h.drv.CheckCPU(),
		h.drv.CheckDriver(),
		identity[3],
		h.drv.CheckReadback(),
		h.drv.CheckSlotMapping(),
		h.checkBackends(),
		h.checkBackendUser(),
		systemdRun,
	}
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
