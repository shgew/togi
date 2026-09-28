package hardware

import (
	"fmt"
	"os"
	"path/filepath"
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
	h := &host{drv: drv, cfg: cfg, backends: backends}
	return machine.Machine{
		Clock:  clock{},
		SMU:    drv,
		Host:   h,
		Trials: trial.New(trial.Options{Dir: filepath.Join(stateDir, "trials"), Backends: backends, Cores: cores}),
		Kernel: detect.NewKernel(cores),
	}, nil
}

type clock struct{}

func (clock) Now() time.Time { return time.Now() }

type host struct {
	drv      *smu.Driver
	cfg      config.Config
	backends map[machine.Backend]backend.Backend
}

func (h *host) BootID() (string, error) { return detect.BootID() }

func (h *host) Topology() ([]machine.CoreInfo, error) { return h.drv.Topology(), nil }

func (h *host) BIOSContext() (machine.BIOSContext, error) { return h.drv.BIOSContext() }

func (h *host) Preflight() []machine.Check {
	root := machine.Check{Name: "root", Detail: "uid 0", OK: true}
	if uid := os.Geteuid(); uid != 0 {
		root = machine.Check{Name: "root", Detail: fmt.Sprintf("running as uid %d; run needs root", uid)}
	}
	systemdRun := machine.Check{Name: "systemd_run", OK: true}
	detail, err := trial.CheckSystemdRun()
	systemdRun.Detail = detail
	if err != nil {
		systemdRun.Detail, systemdRun.OK = err.Error(), false
	}
	return []machine.Check{
		root,
		h.drv.CheckCPU(),
		h.drv.CheckDriver(),
		h.drv.CheckReadback(),
		h.drv.CheckSlotMapping(),
		h.checkBackends(),
		systemdRun,
	}
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
