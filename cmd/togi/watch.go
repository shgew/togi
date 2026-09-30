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

	"github.com/shgew/togi/internal/watch"
)

const watchHelp = `Usage: togi watch [--width <columns>] [--height <rows>]

Show the session as a dashboard that redraws every second: search, hunt,
refinement or guard activity, the running trial, one tile per core with its
offset, failed and joint marks, and the latest events. Masked trials show their
anchor offsets; DONE, HUNT and MASK distinguish the current activity. Read-only;
rendered from the journal, which it reloads when it changes. On a terminal it
fills the screen until interrupted; otherwise it prints one frame of --width
by --height and exits. An unreadable or incompatible journal appears in the
frame and on stderr, and one-frame watch exits 1; live watch keeps showing the
problem. No session yet is not an error. The tuning boot shows it on tty1.
NO_COLOR turns colour off.

Examples:
  togi watch                                  The session in the default state directory, full screen
  togi --state-dir <dir> watch > frame.txt    One 240x67 frame of the session in <dir>`

func runWatch(g *globals, args []string, stdout, stderr io.Writer) int {
	flags := newFlagSet("watch", g)
	width := flags.Int("width", 240, "frame width in `columns` when stdout is not a terminal")
	height := flags.Int("height", 67, "frame height in `rows` when stdout is not a terminal")
	if code, ok := parseFlags(flags, args, watchHelp, stdout, stderr); !ok {
		return code
	}
	if *width < 1 || *height < 1 {
		fmt.Fprintln(stderr, "togi watch: --width and --height must be at least 1")
		commandUsage(flags, watchHelp, stderr)
		return exitUsage
	}
	if f, ok := stdout.(*os.File); ok && term.IsTerminal(int(f.Fd())) {
		ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM, syscall.SIGHUP)
		defer stop()
		if err := watch.Run(ctx, g.stateDir, f); err != nil {
			fmt.Fprintf(stderr, "togi watch: %v\n", err)
			return exitError
		}
		return exitOK
	}
	snapshot := watch.Load(g.stateDir)
	_, _ = io.WriteString(stdout, ansi.Strip(watch.Render(snapshot, *width, *height, time.Now()))+"\n")
	if err := snapshot.Err(); err != nil {
		fmt.Fprintf(stderr, "togi watch: %s\n", ansi.Strip(err.Error()))
		return exitError
	}
	return exitOK
}
