package main

import (
	"context"
	"fmt"
	"io"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/charmbracelet/x/ansi"
	"golang.org/x/term"

	"code.marleb.org/shgew/shycler/internal/watch"
)

const watchHelp = `Usage: shycler watch [--width <columns>] [--height <rows>]

Show the session as a dashboard that redraws every second: the stage, the
running trial, one tile per core with its offset, failed mark and regainable or
settled depth, and the latest events. Read-only; rendered from the journal,
which it reloads when it changes. On a terminal it fills the screen until
interrupted; otherwise it prints one frame of --width by --height and exits.
The tuning boot shows it on tty1. A different schema shows the refusal instead
of the session. NO_COLOR turns colour off.

Examples:
  shycler watch                                  The session in the default state directory, full screen
  shycler --state-dir <dir> watch > frame.txt    One 240x67 frame of the session in <dir>`

func runWatch(g *globals, args []string, stdout, stderr io.Writer) int {
	flags := newFlagSet("watch", g)
	width := flags.Int("width", 240, "frame width in `columns` when stdout is not a terminal")
	height := flags.Int("height", 67, "frame height in `rows` when stdout is not a terminal")
	if code, ok := parseFlags(flags, args, watchHelp, stdout, stderr); !ok {
		return code
	}
	if *width < 1 || *height < 1 {
		fmt.Fprintln(stderr, "shycler watch: --width and --height must be at least 1")
		commandUsage(flags, watchHelp, stderr)
		return exitUsage
	}
	if f, ok := stdout.(*os.File); ok && term.IsTerminal(int(f.Fd())) {
		ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM, syscall.SIGHUP)
		defer stop()
		if err := watch.Run(ctx, g.stateDir, f); err != nil {
			fmt.Fprintf(stderr, "shycler watch: %v\n", err)
			return exitError
		}
		return exitOK
	}
	_, _ = io.WriteString(stdout, ansi.Strip(watch.Render(watch.Load(g.stateDir), *width, *height, time.Now()))+"\n")
	return exitOK
}
