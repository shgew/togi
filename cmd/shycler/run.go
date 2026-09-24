package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"os/signal"
	"strconv"
	"syscall"
	"time"

	"code.marleb.org/shgew/shycler/internal/config"
	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/session"
	"code.marleb.org/shgew/shycler/internal/sim"
)

const runHelp = `Usage: shycler run [--sim <seed>] [--rotations <N>]

Start or resume the tuning session in the foreground: search each core's deepest
stable offset, confirm it, then keep guarding all offsets together. After a crash,
the next run attributes it from the journal and continues. Until hardware support
lands, run needs --sim.

Examples:
  shycler run --sim 1                  Simulate a session through its first clean guard rotation
  shycler run --sim 1 --rotations 20   Keep the simulated guard going for 20 clean rotations`

func runRun(g *globals, args []string, stdout, stderr io.Writer) int {
	var (
		seed         uint64
		seedSet      bool
		rotations    int
		rotationsSet bool
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
	if code, ok := parseFlags(flags, args, runHelp, stdout, stderr); !ok {
		return code
	}

	cfg, file, err := loadConfig(g)
	if err != nil {
		fmt.Fprintf(stderr, "shycler run: %v\n", err)
		return exitUsage
	}
	if !seedSet {
		fmt.Fprintln(stderr, "shycler run: running on hardware is not implemented yet; use --sim <seed>")
		return exitUsage
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
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM, syscall.SIGHUP)
	defer cancel()
	stop, err := session.Simulate(ctx, session.SimInput{Config: cfg, ConfigPath: g.config, ConfigFile: file, Dir: dir, Machine: m, Log: stderr, Rotations: rotations})
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
