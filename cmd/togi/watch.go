package main

import (
	"context"
	"flag"
	"fmt"
	"io"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/charmbracelet/x/ansi"
	"golang.org/x/term"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/watch"
)

const watchHelp = `Usage: togi watch [--width <columns>] [--height <rows>]

Show the session as a dashboard that redraws every second. togi says in plain
words what it is doing and why, how far the current test and cycle have come,
and the stages from finding each core's limit to a clean cycle; a HUNT lamp
lights while a hunt pauses them. One row per core shows the offset applied
now, where the core stands and how deep it goes; the cores the test is judging
are bright, the others grey. Below them, what happened recently. On a
terminal, keys switch views: ? explains everything on the screen, L shows the
event log as the journal records it, the arrow and page keys scroll both, q
quits. Read-only; rendered from the journal, which it reloads when it changes.
On a terminal it fills the screen until interrupted; otherwise it prints one
frame of --width by --height and exits. An unreadable or incompatible journal
appears in the frame and on stderr, and one-frame watch exits 1; live watch
keeps showing the problem. No session yet is not an error. The tuning boot
shows it on tty1. NO_COLOR turns colour off.

Examples:
  togi watch                                  The session in the default state directory, full screen
  togi --state-dir <dir> watch > frame.txt    One 240x67 frame of the session in <dir>`

func watchFlags(g *globals, width, height *int) *flag.FlagSet {
	flags := newFlagSet("watch", g)
	flags.IntVar(width, "width", 240, "frame width in `columns` when stdout is not a terminal")
	flags.IntVar(height, "height", 67, "frame height in `rows` when stdout is not a terminal")
	return flags
}

func runWatch(g *globals, args []string, stdout, stderr io.Writer) int {
	var width, height int
	flags := watchFlags(g, &width, &height)
	if code, ok := parseFlags(flags, args, watchHelp, stdout, stderr); !ok {
		return code
	}
	if width < 1 || height < 1 {
		fmt.Fprintln(stderr, "togi watch: --width and --height must be at least 1")
		commandUsage(flags, watchHelp, stderr)
		return exitUsage
	}
	if f, ok := stdout.(*os.File); ok && term.IsTerminal(int(f.Fd())) {
		ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM, syscall.SIGHUP)
		defer stop()
		if err := watch.Run(ctx, g.stateDir, f, os.Stdin); err != nil {
			fmt.Fprintf(stderr, "togi watch: %v\n", err)
			return exitError
		}
		return exitOK
	}
	snapshot := watch.Load(g.stateDir)
	_, _ = io.WriteString(stdout, ansi.Strip(watch.Render(snapshot, width, height, time.Now()))+"\n")
	if err := snapshot.Err(); err != nil {
		fmt.Fprintf(stderr, "togi watch: %s\n", journal.EscapeText(err.Error()))
		return exitError
	}
	return exitOK
}
