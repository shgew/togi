// Package hardware assembles the real machine: host, preflight and GRUB.
package hardware

import (
	"bytes"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"code.marleb.org/shgew/shycler/internal/backend"
	"code.marleb.org/shgew/shycler/internal/backend/mprime"
	"code.marleb.org/shgew/shycler/internal/backend/ycruncher"
	"code.marleb.org/shgew/shycler/internal/config"
	"code.marleb.org/shgew/shycler/internal/detect"
	"code.marleb.org/shgew/shycler/internal/machine"
	"code.marleb.org/shgew/shycler/internal/smu"
	"code.marleb.org/shgew/shycler/internal/trial"
)

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

// GRUB clears the saved entry in the GRUB environment file Env with grub-editenv.
type GRUB struct{ Env string }

func (g GRUB) ClearSavedEntry() (before, after string, err error) {
	if before, err = g.savedEntry(); err != nil {
		return "", "", err
	}
	if _, err := g.editenv("unset", "saved_entry"); err != nil {
		return before, before, err
	}
	if after, err = g.savedEntry(); err != nil {
		return before, "", err
	}
	if after != "" {
		return before, after, fmt.Errorf("saved_entry still %s after unset", after)
	}
	return before, "", nil
}

func (g GRUB) savedEntry() (string, error) {
	out, err := g.editenv("list")
	if err != nil {
		return "", err
	}
	for line := range strings.Lines(out) {
		if v, ok := strings.CutPrefix(strings.TrimSpace(line), "saved_entry="); ok {
			return v, nil
		}
	}
	return "", nil
}

func (g GRUB) editenv(args ...string) (string, error) {
	cmd := exec.Command("grub-editenv", append([]string{g.Env}, args...)...)
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if err := cmd.Run(); err != nil {
		msg := strings.TrimSpace(stderr.String())
		if msg == "" {
			return "", fmt.Errorf("grub-editenv %s %s: %w", g.Env, args[0], err)
		}
		return "", fmt.Errorf("grub-editenv %s %s: %w: %s", g.Env, args[0], err, msg)
	}
	return stdout.String(), nil
}
