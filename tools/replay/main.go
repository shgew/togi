// replay plays a recorded journal through the dashboard on a simulated clock, to try the dashboard on a whole session
// in minutes. It is a development program and never ships.
package main

import (
	"context"
	"flag"
	"fmt"
	"io"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	"github.com/charmbracelet/colorprofile"
	"github.com/charmbracelet/x/ansi"
	"golang.org/x/term"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/watch"
)

func main() {
	if err := run(os.Args[1:], os.Stdout, os.Stderr); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run(args []string, out *os.File, errOut io.Writer) error {
	flags := flag.NewFlagSet("replay", flag.ContinueOnError)
	flags.SetOutput(errOut)
	dir := flags.String("state-dir", "", "state directory whose events.jsonl to play")
	speed := flags.Float64("speed", 300, "simulated seconds per wall-clock second")
	from := flags.Int("from", 1, "start playing at this event `seq`")
	at := flags.Int("at", 0, "print one frame as of this event `seq` and exit")
	after := flags.Duration("after", 0, "with --at, how long after that event the frame is drawn")
	width := flags.Int("width", 240, "frame width for --at")
	height := flags.Int("height", 67, "frame height for --at")
	color := flags.Bool("color", false, "keep ANSI colour in the --at frame")
	view := flags.String("view", "main", "with --at, the view to draw: main, help or log")
	if err := flags.Parse(args); err != nil {
		return err
	}
	if *dir == "" || flags.NArg() != 0 || *speed <= 0 {
		return fmt.Errorf("replay: --state-dir is required and --speed must be positive")
	}
	events, _, err := journal.Read(*dir)
	if err != nil {
		return fmt.Errorf("replay: read journal: %w", err)
	}
	if len(events) == 0 {
		return fmt.Errorf("replay: %s holds no events", *dir)
	}
	if *at > 0 {
		v, ok := map[string]watch.View{"main": watch.MainView, "help": watch.HelpView, "log": watch.LogView}[*view]
		if !ok {
			return fmt.Errorf("replay: --view must be main, help or log")
		}
		return frame(out, events, prefix(events, *at), v, *after, *width, *height, *color)
	}
	return play(out, events, prefix(events, *from), *speed)
}

// prefix is how many events come at or before seq.
func prefix(events []journal.Event, seq int) int {
	n := 0
	for n < len(events) && events[n].Seq <= seq {
		n++
	}
	return max(n, 1)
}

func frame(out io.Writer, events []journal.Event, n int, v watch.View, after time.Duration, w, h int, color bool) error {
	sc := watch.Screen{View: v, Keys: true, Width: w, Height: h}
	if v == watch.LogView {
		sc.Scroll = -1
	}
	text, _ := watch.RenderView(watch.Project(events[:n]), sc, events[n-1].Time.Add(after))
	if !color {
		text = ansi.Strip(text)
	} else {
		var b strings.Builder
		cw := &colorprofile.Writer{Forward: &b, Profile: colorprofile.ANSI}
		_, _ = cw.Write([]byte(text))
		text = b.String()
	}
	_, err := io.WriteString(out, text+"\n")
	return err
}

func play(out *os.File, events []journal.Event, n int, speed float64) error {
	if !term.IsTerminal(int(out.Fd())) {
		return fmt.Errorf("replay: playing needs a terminal; use --at for one frame")
	}
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	tick := time.NewTicker(250 * time.Millisecond)
	defer tick.Stop()
	start, wall := events[n-1].Time, time.Now()
	shown := -1
	var snap watch.Snapshot
	return watch.Show(ctx, out, os.Stdin, tick.C, func(sc watch.Screen) (string, int) {
		clock := start.Add(time.Duration(float64(time.Since(wall)) * speed))
		for n < len(events) && !events[n].Time.After(clock) {
			n++
		}
		if n != shown {
			snap, shown = watch.Project(events[:n]), n
		}
		return watch.RenderView(snap, sc, clock)
	})
}
