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

	"github.com/shgew/togi/internal/render"
	"github.com/shgew/togi/internal/watch"
)

var watchHelp = `Usage: togi watch [--width <columns>] [--height <rows>]

Show the session as a read-only dashboard. It redraws when the journal changes
and once a second for the clock. During an idle trial it holds still, clock
included, until the journal changes, a key is pressed or the terminal is
resized. togi says in plain words what it is doing and why, how far the
current trial and cycle have come, and what each outcome would lead to. Each
CCD's cores show their offsets and limits side by side; below them, the cycle,
hunt or search beside what happened recently. On a terminal, keys switch
views: ? explains the screen, l shows the newest 400 journal events, the arrow
and page keys scroll both, q quits. A journal scrolled back holds its list
still and counts new events until End. Rendered from the journal, which it
reloads only when it changes.
On a terminal it fills the screen until interrupted; otherwise it prints one
frame of --width by --height and exits. An unreadable or incompatible journal
appears in the frame and on stderr, and one-frame watch exits 1; live watch
keeps showing the problem. No session yet is not an error. The tuning boot
shows it on tty1. NO_COLOR turns colour off.

` + examples(
	example{"togi watch", "The session in the default state directory, full screen"},
	example{"togi --state-dir <dir> watch > frame.txt", "One 240x67 frame of the session in <dir>"},
)

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
		fmt.Fprintf(stderr, "togi watch: %s\n", render.EscapeText(err.Error()))
		return exitError
	}
	return exitOK
}
