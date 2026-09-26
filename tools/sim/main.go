// Command sim runs a seeded simulated session from a source checkout, without hardware or root.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/shgew/shycler/internal/config"
	"github.com/shgew/shycler/internal/journal"
	"github.com/shgew/shycler/internal/session"
	"github.com/shgew/shycler/internal/sim"
	"github.com/shgew/shycler/internal/simrun"
)

func main() {
	os.Exit(run(os.Args[1:], os.Stderr))
}

func run(args []string, stderr io.Writer) int {
	flags := flag.NewFlagSet("sim", flag.ContinueOnError)
	flags.SetOutput(stderr)
	seed := flags.Uint64("seed", 1, "draw the simulated machine's edges and failures from this `seed`")
	rotations := flags.Int("rotations", 1, "stop after `N` clean guard rotations of one profile")
	dir := flags.String("state-dir", "", "use this state `directory`, resuming a journal it holds; default a new temporary one")
	if err := flags.Parse(args); errors.Is(err, flag.ErrHelp) {
		return 0
	} else if err != nil {
		return 2
	}
	if flags.NArg() != 0 {
		fmt.Fprintln(stderr, "sim: unexpected positional arguments")
		return 2
	}
	if *rotations < 1 {
		fmt.Fprintln(stderr, "sim: --rotations must be a positive integer")
		return 2
	}
	if *dir == "" {
		var err error
		if *dir, err = os.MkdirTemp("", "shycler-sim-"); err != nil {
			fmt.Fprintf(stderr, "sim: %v\n", err)
			return 1
		}
		fmt.Fprintf(stderr, "sim: state directory %s\n", *dir)
	}
	cfg, err := sim.Resume(*dir, sim.Config{Seed: *seed})
	if err != nil {
		fmt.Fprintf(stderr, "sim: %v\n", err)
		return 1
	}
	m, err := sim.New(cfg)
	if err != nil {
		fmt.Fprintf(stderr, "sim: %v\n", err)
		return 1
	}
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM, syscall.SIGHUP)
	defer cancel()
	renderer := journal.NewRenderer(stderr, os.Getenv)
	stop, err := simrun.Simulate(ctx, simrun.Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: *dir, Machine: m, Log: stderr, Renderer: renderer, Rotations: *rotations})
	if err != nil {
		fmt.Fprintf(stderr, "sim: %v\n", err)
		return 1
	}
	if stop.Reason != session.StopDeadEnd {
		return 0
	}
	line := fmt.Sprintf("sim: dead end %s: %s", stop.DeadEnd.Condition, stop.DeadEnd.Detail)
	fmt.Fprintln(stderr, renderer.Text(journal.Event{Kind: journal.KindDeadEnd, Data: stop.DeadEnd}, line))
	for _, e := range stop.Evidence {
		fmt.Fprintln(stderr, renderer.Text(e, "  evidence: "+journal.FormatLine(e, time.Local)))
	}
	return 1
}
