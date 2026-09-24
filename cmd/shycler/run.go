package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"os/exec"
	"os/signal"
	"strconv"
	"strings"
	"syscall"
	"time"

	"code.marleb.org/shgew/shycler/internal/config"
	"code.marleb.org/shgew/shycler/internal/detect"
	"code.marleb.org/shgew/shycler/internal/hardware"
	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/session"
	"code.marleb.org/shgew/shycler/internal/sim"
)

const runHelp = `Usage: shycler run [--sim <seed>] [--rotations <N>] [--tuning-boot <grubenv>]

Start or resume the tuning session in the foreground: search each core's deepest
stable offset, confirm it, then keep guarding all offsets together. After a crash,
the next run attributes it from the journal and continues. On hardware it needs
root; --sim drives a simulated machine instead.

Examples:
  sudo shycler run                     Tune this machine until a signal or a dead end
  sudo shycler run --rotations 1       Stop after the first clean guard rotation
  shycler run --sim 1                  Simulate a session through its first clean guard rotation`

func runRun(g *globals, args []string, stdout, stderr io.Writer) int {
	var (
		seed         uint64
		seedSet      bool
		rotations    int
		rotationsSet bool
		grubenv      string
	)
	flags := newFlagSet("run", g)
	flags.Func("sim", "drive a simulated 16-core machine with this `seed` instead of hardware", func(s string) error {
		v, err := strconv.ParseUint(s, 10, 64)
		if err != nil {
			return errors.New("must be a non-negative integer")
		}
		seed, seedSet = v, true
		return nil
	})
	flags.Func("rotations", "stop after `N` clean guard rotations of one profile (default 1 with --sim, else endless)", func(s string) error {
		v, err := strconv.Atoi(s)
		if err != nil || v < 1 {
			return errors.New("must be a positive integer")
		}
		rotations, rotationsSet = v, true
		return nil
	})
	flags.StringVar(&grubenv, "tuning-boot", "", "run as the tuning boot service: at a dead end clear saved_entry in this GRUB environment `file`, and reboot after a boot loop")
	if code, ok := parseFlags(flags, args, runHelp, stdout, stderr); !ok {
		return code
	}
	if seedSet && grubenv != "" {
		fmt.Fprintln(stderr, "shycler run: --sim and --tuning-boot cannot be combined: a simulated dead end must not change the host's boot entry")
		commandUsage(flags, runHelp, stderr)
		return exitUsage
	}

	cfg, file, err := loadConfig(g)
	if err != nil {
		fmt.Fprintf(stderr, "shycler run: %v\n", err)
		return exitUsage
	}
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM, syscall.SIGHUP)
	defer cancel()
	if !seedSet {
		var bootloader session.Bootloader
		if grubenv != "" {
			bootloader = hardware.GRUB{Env: grubenv}
		}
		return runHardware(ctx, g, cfg, file, bootloader, rotations, stderr)
	}
	dir := g.stateDir
	if !g.stateDirSet {
		if dir, err = os.MkdirTemp("", "shycler-sim-"); err != nil {
			fmt.Fprintf(stderr, "shycler run: %v\n", err)
			return exitError
		}
		fmt.Fprintf(stderr, "shycler run: simulated state directory %s\n", dir)
	}
	if !rotationsSet {
		rotations = 1
	}
	simCfg, err := session.Resume(dir, sim.Config{Seed: seed})
	if err != nil {
		fmt.Fprintf(stderr, "shycler run: %v\n", err)
		return exitError
	}
	m, err := sim.New(simCfg)
	if err != nil {
		fmt.Fprintf(stderr, "shycler run: %v\n", err)
		return exitError
	}
	stop, err := session.Simulate(ctx, session.SimInput{Config: cfg, ConfigPath: g.config, ConfigFile: file, Dir: dir, Machine: m, Log: stderr, Rotations: rotations})
	return runResult(stop, err, stderr)
}

func runHardware(ctx context.Context, g *globals, cfg config.Config, file bool, bootloader session.Bootloader, rotations int, stderr io.Writer) int {
	boot, err := detect.BootID()
	if err != nil {
		fmt.Fprintf(stderr, "shycler run: %v\n", err)
		return exitError
	}
	m, err := hardware.New(cfg, g.stateDir)
	if err != nil {
		fmt.Fprintf(stderr, "shycler run: %v\n", err)
		return exitError
	}
	j, err := journal.Open(g.stateDir, journal.Options{Boot: boot, Sync: true, Log: stderr})
	if err != nil {
		return runResult(session.Stop{}, err, stderr)
	}
	stop, err := session.Run(ctx, session.Input{Config: cfg, ConfigPath: g.config, ConfigFile: file, Boot: boot, Journal: j, Machine: m, Rotations: rotations, Bootloader: bootloader})
	if cerr := j.Close(); err == nil && cerr != nil {
		err = cerr
	}
	return runResult(stop, err, stderr)
}

func runResult(stop session.Stop, err error, stderr io.Writer) int {
	switch {
	case errors.Is(err, session.ErrNoSuchCore):
		fmt.Fprintf(stderr, "shycler run: %v\n", err)
		return exitUsage
	case errors.Is(err, journal.ErrLocked):
		fmt.Fprintf(stderr, "shycler run: %v\n", err)
		return exitLocked
	case err != nil:
		fmt.Fprintf(stderr, "shycler run: %v\n", err)
		return exitError
	}
	switch stop.Reason {
	case session.StopSignal, session.StopRotations:
		return exitOK
	case session.StopDeadEnd:
		fmt.Fprintf(stderr, "shycler: dead end %s: %s\n", stop.DeadEnd.Condition, stop.DeadEnd.Detail)
		for _, e := range stop.Evidence {
			fmt.Fprintf(stderr, "  evidence: %s\n", journal.FormatLine(e, time.Local))
		}
		if stop.Reboot {
			fmt.Fprintln(stderr, "shycler: rebooting into the normal system")
			if out, err := exec.Command("systemctl", "reboot").CombinedOutput(); err != nil {
				fmt.Fprintf(stderr, "shycler: systemctl reboot: %v: %s\n", err, strings.TrimSpace(string(out)))
			}
		}
		return deadEndExit(stop.DeadEnd.Condition)
	}
	return exitError
}

func loadConfig(g *globals) (config.Config, bool, error) {
	cfg, err := config.Load(g.config)
	if err == nil {
		return cfg, true, nil
	}
	if !g.configSet && errors.Is(err, fs.ErrNotExist) {
		return config.Default(), false, nil
	}
	return config.Config{}, false, err
}
