// replay plays a recorded journal through the dashboard on a simulated clock, to try the dashboard on a whole session
// in minutes. It is a development program and never ships.
package main

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"flag"
	"fmt"
	"io"
	"math"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	"github.com/charmbracelet/colorprofile"
	"github.com/charmbracelet/x/ansi"
	"golang.org/x/term"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/tuner"
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
	speed := flags.Float64("speed", 300, "simulated seconds per wall-clock second (finite and positive)")
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
	if *dir == "" || flags.NArg() != 0 || *speed <= 0 || math.IsNaN(*speed) || math.IsInf(*speed, 0) {
		return fmt.Errorf("replay: --state-dir is required and --speed must be finite and positive")
	}
	events, _, err := journal.ReadReplay(*dir, tuner.Ruleset)
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
	text := strings.Join(watch.RenderView(watch.Project(events[:n]), sc, events[n-1].Time.Add(after)).Lines, "\n")
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

const maxDuration = time.Duration(1<<63 - 1)

type playback struct {
	events         []journal.Event
	speed          float64
	n              int
	at, gap        time.Duration
	mono, nextMono bool
	raw            bytes.Reader
	decoder        *jsontext.Decoder
}

func newPlayback(events []journal.Event, n int, speed float64) *playback {
	p := &playback{events: events, n: n, speed: speed}
	p.decoder = jsontext.NewDecoder(&p.raw)
	p.mono = p.hasMono(events[n-1].Raw)
	p.schedule()
	return p
}

// hasMono reads only top-level names: zero is a valid stamp, and nested stamps are not this event's clock.
func (p *playback) hasMono(raw []byte) bool {
	p.raw.Reset(raw)
	p.decoder.Reset(&p.raw)
	token, err := p.decoder.ReadToken()
	if err != nil || token.Kind() != '{' {
		return false
	}
	for p.decoder.PeekKind() != '}' {
		name, err := p.decoder.ReadToken()
		if err != nil {
			return false
		}
		if name.String() == "mono_ms" {
			return true
		}
		if err := p.decoder.SkipValue(); err != nil {
			return false
		}
	}
	return false
}

func (p *playback) schedule() {
	if p.n == len(p.events) {
		return
	}
	before, after := &p.events[p.n-1], &p.events[p.n]
	p.nextMono = p.hasMono(after.Raw)
	p.gap = max(0, after.Time.Sub(before.Time))
	if before.Boot != "" && before.Boot == after.Boot && p.mono && p.nextMono {
		p.gap = 0
		if after.Mono > before.Mono {
			ms := uint64(after.Mono) - uint64(before.Mono)
			if ms > uint64(maxDuration/time.Millisecond) {
				p.gap = maxDuration
			} else {
				p.gap = time.Duration(ms) * time.Millisecond
			}
		}
	}
}

// advance takes elapsed wall time since playback started, not the journal's adjustable wall clock.
func (p *playback) advance(elapsed time.Duration) (int, time.Time) {
	scaled := float64(elapsed) * p.speed
	if scaled >= float64(maxDuration) {
		elapsed = maxDuration
	} else {
		elapsed = time.Duration(scaled)
	}
	for p.n < len(p.events) && p.gap <= elapsed-p.at {
		p.at += p.gap
		p.n++
		p.mono = p.nextMono
		p.schedule()
	}
	return p.n, p.events[p.n-1].Time.Add(elapsed - p.at)
}

func play(out *os.File, events []journal.Event, n int, speed float64) error {
	if !term.IsTerminal(int(out.Fd())) {
		return fmt.Errorf("replay: playing needs a terminal; use --at for one frame")
	}
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	tick := time.NewTicker(250 * time.Millisecond)
	defer tick.Stop()
	p := newPlayback(events, n, speed)
	wall := time.Now()
	shown := -1
	var snap watch.Snapshot
	return watch.Show(ctx, out, os.Stdin, tick.C, func(sc watch.Screen) watch.Drawn {
		n, clock := p.advance(time.Since(wall))
		if n != shown {
			snap, shown = watch.Project(events[:n]), n
		}
		return watch.RenderView(snap, sc, clock)
	})
}
