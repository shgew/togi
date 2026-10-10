// Command sim runs a seeded simulated session from a source checkout, without hardware or root.
package main

import (
	"bufio"
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/render"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/simrun"
	"github.com/shgew/togi/tools/trialfacts"
)

func main() {
	os.Exit(run(os.Args[1:], os.Stderr))
}

func run(args []string, stderr io.Writer) int {
	flags := flag.NewFlagSet("sim", flag.ContinueOnError)
	flags.SetOutput(stderr)
	seed := flags.Uint64("seed", 1, "draw the simulated machine's limits and failures from this `seed`")
	cycles := flags.Int("cycles", 1, "stop after `N` clean cycles once every core is at its limit and deepening can reach no more depth")
	machineFile := flags.String("machine", "", "load the simulated machine from this JSON `file`")
	replay := flags.Bool("replay-facts", false, "answer exact class/profile matches from the machine's same-BIOS facts extract")
	dir := flags.String("state-dir", "", "use this state `directory`, resuming a journal it holds under the configuration that journal recorded, or under the default configuration with its recorded backend store paths when the journal has an older schema; default a new temporary one")
	samples := flags.Bool("samples", false, "write trials/<trial-id>/samples.jsonl in the state directory; default keep samples in memory")
	maxBoots := flags.Int("max-boots", 1000, "stop with exit status 3 if the session is still running after `N` simulated boots")
	verifyEvery := flags.Int("verify-every", 0, "after every `N`th simulated crash, replay the whole journal into fresh state and fail unless it equals the state the next boot resumes from; 0 checks only when the session stops")
	coldBoots := flags.Bool("cold-boots", false, "replay the whole journal at every boot, as togi run does, instead of resuming from the state the crashed boot folded")
	if err := flags.Parse(args); errors.Is(err, flag.ErrHelp) {
		return 0
	} else if err != nil {
		return 2
	}
	if flags.NArg() != 0 {
		fmt.Fprintln(stderr, "sim: unexpected positional arguments")
		return 2
	}
	if *cycles < 1 {
		fmt.Fprintln(stderr, "sim: --cycles must be a positive integer")
		return 2
	}
	if *verifyEvery < 0 {
		fmt.Fprintln(stderr, "sim: --verify-every must not be negative")
		return 2
	}
	if *maxBoots < 1 {
		fmt.Fprintln(stderr, "sim: --max-boots must be a positive integer")
		return 2
	}
	machineConfig := sim.Config{Seed: *seed}
	var err error
	if *machineFile != "" {
		machineConfig, err = sim.LoadMachine(*machineFile)
		if err != nil {
			fmt.Fprintf(stderr, "sim: load machine: %v\n", err)
			return 1
		}
		machineConfig.Seed = *seed
	}
	if *replay {
		machineConfig.Replay, err = trialfacts.Extracts{}.Replay(*machineFile, machineConfig)
		if err != nil {
			fmt.Fprintf(stderr, "sim: %v\n", err)
			return 1
		}
	}
	if *dir == "" {
		if *dir, err = os.MkdirTemp("", "togi-sim-"); err != nil {
			fmt.Fprintf(stderr, "sim: %v\n", err)
			return 1
		}
		fmt.Fprintf(stderr, "sim: state directory %s\n", *dir)
	}
	cfg, err := sim.Resume(*dir, machineConfig)
	if err != nil {
		fmt.Fprintf(stderr, "sim: %v\n", err)
		return 1
	}
	recorded, err := simrun.RecordedConfig(*dir, config.Default())
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
	renderer := render.NewRenderer(stderr, os.Getenv)
	log, flush := bufferedLog(stderr)
	defer flush()
	stop, err := simrun.Simulate(ctx, simrun.Input{Config: recorded, ConfigPath: config.DefaultPath, Dir: *dir, Machine: m, Log: log, Renderer: renderer, Cycles: *cycles, InMemoryJournal: true, WriteSamples: *samples, MaxBoots: *maxBoots, ColdBoots: *coldBoots, VerifyEvery: *verifyEvery})
	if errors.Is(err, simrun.ErrBootCap) {
		fmt.Fprintf(log, "sim: %v\n", err)
		return 3
	}
	if err != nil {
		fmt.Fprintf(log, "sim: %v\n", err)
		return 1
	}
	if stop.Reason != session.StopDeadEnd {
		return 0
	}
	line := fmt.Sprintf("sim: dead end %s: %s", stop.DeadEnd.Condition, stop.DeadEnd.Detail)
	fmt.Fprintln(log, renderer.Text(journal.Event{Kind: journal.KindDeadEnd, Data: stop.DeadEnd}, line))
	for _, e := range stop.Evidence {
		fmt.Fprintln(log, renderer.PrefixedLine(e, time.Local, "  evidence: "))
	}
	return 1
}

// bufferedLog returns w for a terminal, where the log has to keep pace with the run, and otherwise a buffer in front of
// w that the returned function flushes: the run writes one line per event, a write call each.
func bufferedLog(w io.Writer) (io.Writer, func()) {
	if f, ok := w.(*os.File); ok {
		if info, err := f.Stat(); err == nil && info.Mode()&os.ModeCharDevice != 0 {
			return w, func() {}
		}
	}
	buffer := bufio.NewWriterSize(w, 64<<10)
	return buffer, func() { _ = buffer.Flush() }
}
