package watch

import (
	"fmt"
	"math"
	"slices"
	"strings"
	"time"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

// The dashboard uses the 16 console colour slots only, never bold: the Linux console turns bold into a colour change.
// On the console, palette() redefines the slots to the shades below.
var (
	plain     = lipgloss.NewStyle()
	textStyle = plain.Foreground(lipgloss.Color("7"))
	grey      = plain.Foreground(lipgloss.Color("8"))
	white     = plain.Foreground(lipgloss.Color("15"))
	lit       = plain.Foreground(lipgloss.Color("14"))
	litTrack  = plain.Foreground(lipgloss.Color("6"))
	amber     = plain.Foreground(lipgloss.Color("11"))
	green     = plain.Foreground(lipgloss.Color("10"))
	red       = plain.Foreground(lipgloss.Color("9"))
	blue      = plain.Foreground(lipgloss.Color("12"))
	chip      = plain.Foreground(lipgloss.Color("0")).Background(lipgloss.Color("7"))
	lamp      = plain.Foreground(lipgloss.Color("0")).Background(lipgloss.Color("3"))
)

func toneStyle(t tone) lipgloss.Style {
	switch t {
	case goodTone:
		return green
	case badTone:
		return red
	case warnTone:
		return amber
	case plainTone:
	}
	return textStyle
}

// View selects what fills the screen.
type View int

const (
	MainView View = iota
	HelpView
	LogView
)

// Screen is what a frame shows and at what size. Scroll is how many lines the help or the log is scrolled from its
// top; a negative Scroll shows its end. Keys says whether the viewer can switch views and scroll from the keyboard.
type Screen struct {
	View          View
	Keys          bool
	Scroll        int
	Width, Height int
}

// Every element of a frame shares one width and the text wraps to it: 80 columns, the most a line of prose should run
// (WCAG 1.4.8), and exactly a core row with one gauge cell per count. It is a count of characters, not a share of the
// screen, so a wider screen leaves room to the right rather than longer lines, and nothing moves as the content
// changes. Only the event log, one unwrapped line per event, runs to the edge of the screen.
const (
	margin     = 2
	frameWidth = 80
	minWidth   = 20
	gaugeCells = 50
	rowText    = 30
)

var pad = strings.Repeat(" ", margin)

// Render draws the main view on a w by h screen: exactly h-1 lines, each at most w-1 cells wide, so the last row and
// column are never written and the screen never scrolls.
func Render(s Snapshot, w, h int, now time.Time) string {
	frame, _ := RenderView(s, Screen{View: MainView, Width: w, Height: h}, now)
	return frame
}

// RenderView draws sc and reports how many lines its view can scroll.
func RenderView(s Snapshot, sc Screen, now time.Time) (string, int) {
	screen := max(sc.Width, minWidth) - 1
	width := min(screen-margin, frameWidth)
	lines := []string{pad + s.header(now), ""}
	footer := 0
	if sc.Keys {
		footer = 2
	}
	room := sc.Height - 1 - footer
	hint, scrolled := "? what is all this    L event log    q quit", 0
	switch sc.View {
	case HelpView, LogView:
		if p := s.progress(now); p != "" {
			lines = append(lines, pad+p, "")
		}
		var body []string
		if sc.View == LogView {
			body, hint = s.logLines(screen-margin), "L or Esc back    ? what is all this    q quit"
		} else {
			body, hint = helpLines(width), "? or Esc back    L event log    q quit"
		}
		page := max(room-len(lines), 1)
		scrolled = max(len(body)-page, 0)
		top := sc.Scroll
		if top < 0 || top > scrolled {
			top = scrolled
		}
		bar := scrollbar(page, len(body), top, scrolled)
		for i, l := range body[top:min(len(body), top+page)] {
			lines = append(lines, bar[i]+" "+l)
		}
		if scrolled > 0 {
			hint = fmt.Sprintf("↑↓ PgUp PgDn  lines %d-%d of %d    %s", top+1, min(len(body), top+page), len(body), hint)
		}
	case MainView:
		lines = append(lines, s.mainLines(width, room-len(lines), now)...)
		if rows := room - len(lines) - 1; rows > 2 {
			lines = append(lines, "")
			for _, l := range s.historyLines(width, rows) {
				lines = append(lines, pad+l)
			}
		}
	}
	if sc.Keys && sc.Height >= 2 {
		for len(lines) < sc.Height-2 {
			lines = append(lines, "")
		}
		lines = append(lines[:min(len(lines), sc.Height-2)], pad+grey.Render(hint))
	}
	return fit(lines, sc.Width-1, sc.Height), scrolled
}

// header is the top line of every view: the clock, how long the session has run and how often it failed.
func (s Snapshot) header(now time.Time) string {
	line := chip.Render(" togi ") + "  " + white.Render(now.Format("15:04:05")) + "  "
	if !s.session {
		return line + grey.Render("no session yet")
	}
	switch {
	case s.deadEnd != nil:
		line += grey.Render("stopped at a dead end after ") + white.Render(age(s.deadEnd.at.Sub(s.start)))
	case s.stopped != nil:
		line += grey.Render("stopped after ") + white.Render(age(s.stopped.Sub(s.start)))
	default:
		line += grey.Render("running ") + white.Render(age(now.Sub(s.start)))
	}
	return line + "  " + textStyle.Render(plural(s.failures, "failure")+"  "+plural(s.crashes, "crash"))
}

// progress is the line the help and log views show under the header, so the session stays in sight.
func (s Snapshot) progress(now time.Time) string {
	if !s.session || s.problem != nil {
		return ""
	}
	st := s.story(now)
	line := toneStyle(st.tone).Render(st.headline)
	if n := st.now; n != nil {
		line += "  " + white.Render(n.what)
		if n.timed {
			line += "  " + n.countdown(24)
		}
	}
	return line
}

// mainLines is the main view above what happened. Each line carries its own margin, so the core rows can mark the
// cores under load in it.
func (s Snapshot) mainLines(width, height int, now time.Time) []string {
	var out []string
	add := func(lines ...string) {
		for _, l := range lines {
			out = append(out, pad+l)
		}
	}
	st := s.story(now)
	if !s.session || s.problem != nil {
		add(narrator(width, st, false)...)
		return out
	}
	intro := narrator(width, st, false)
	stages, cores := track(s.stations(), width), s.coreRows(width)
	if len(intro)+len(stages)+6+len(cores) <= height {
		add(intro...)
		add("", "")
		add(stages...)
		add("", s.huntLamp(), "", "")
	} else {
		// Prose and spacing must not push every applied offset below an unscrollable main view.
		intro = narrator(width, st, true)
		if (s.stopped != nil || s.deadEnd != nil) && (s.deadEnd == nil || s.deadEnd.condition != journal.DeadEndSMU) {
			note := "Tuned offsets, not applied now."
			if width-3 < len(note) {
				note = "Saved, not set"
			}
			intro = append(intro, blue.Render("█")+"  "+textStyle.Render(note))
		}
		rows := max(height-len(stages)-len(cores)-2, 2)
		add(intro[:min(len(intro), rows)]...)
		add(stages...)
		add(s.huntLamp(), "")
	}
	out = append(out, cores...)
	if next := s.comingUp(); len(next) > 0 {
		add("", grey.Render("Coming up"))
		for _, n := range next {
			add(wrapStyled(n, width, textStyle)...)
		}
	}
	return out
}

// narrator draws what togi says beside a solid vertical slab, wrapped to the frame.
func narrator(width int, st story, compact bool) []string {
	tw := width - 3
	lines := []string{toneStyle(st.tone).Render(st.headline)}
	if !compact {
		lines = append(lines, "")
		for i, p := range st.paragraphs {
			if i > 0 {
				lines = append(lines, "")
			}
			lines = append(lines, wrapStyled(p, tw, textStyle)...)
		}
	}
	if n := st.now; n != nil {
		detail := textStyle.Render(n.detail)
		if n.backend != "" {
			detail += grey.Render(" (" + n.backend + ")")
		}
		if !compact {
			lines = append(lines, "")
		}
		lines = append(lines, grey.Render("now  ")+white.Render(n.what), ansi.Truncate(detail, tw, ""))
		if n.timed {
			cells := 40
			if compact {
				cells = min(cells, max(tw-12, 1))
			}
			lines = append(lines, n.countdown(cells))
		}
	}
	slab := blue.Render("█")
	out := make([]string, len(lines))
	for i, l := range lines {
		out[i] = slab + "  " + l
	}
	return out
}

func bar(cells int, p float64, fill lipgloss.Style) string {
	k := int(math.Round(float64(cells) * min(max(p, 0), 1)))
	return fill.Render(strings.Repeat("█", k)) + grey.Render(strings.Repeat("░", cells-k))
}

// countdown is the running test's progress bar and the time it has left.
func (n *nowLine) countdown(cells int) string {
	return bar(cells, n.progress, lit) + "  " + white.Render(clock(n.left)+" left")
}

// track draws the stages side by side, each with its name, a progress bar and up to two lines beneath; on a narrow
// screen, the names wrap across lines and the current stage's bar sits beneath.
func track(stations []station, width int) []string {
	span := width / len(stations)
	cells := span - 3
	compact := cells < 12
	rows := make([]string, 4)
	var currentRows []string
	if compact {
		rows = rows[:1]
	}
	for _, st := range stations {
		mark, style, fillStyle, fill := "○", grey, grey, 0.0
		switch st.state {
		case reached:
			mark, style, fillStyle, fill = "■", green, green, 1
		case current:
			mark, style, fillStyle, fill = "►", amber, amber, st.fill
		case target:
			style = white
		case endless:
			mark = "∞"
		case upcoming:
		}
		if compact {
			label := style.Render(mark + " " + st.label)
			j := len(rows) - 1
			if rows[j] != "" {
				if ansi.StringWidth(rows[j])+2+ansi.StringWidth(label) > width {
					rows = append(rows, "")
					j++
				} else {
					rows[j] += "  "
				}
			}
			rows[j] += label
			if st.state == current {
				progress := bar(min(20, width), st.fill, lit) + "  " + textStyle.Render(strings.Join(st.sub, ", "))
				currentRows = []string{progress}
				if ansi.StringWidth(progress) > width {
					currentRows = wrapStyled(progress, width, plain)
				}
			}
			continue
		}
		cols := []string{
			style.Render(ansi.Truncate(mark+" "+st.label, span-1, "")),
			bar(cells, fill, fillStyle),
		}
		for j := range 2 {
			sub, subStyle := "", grey
			if j < len(st.sub) {
				sub = st.sub[j]
			}
			switch {
			case st.paused && j == len(st.sub)-1:
				subStyle = amber
			case st.state == current && j == 0:
				subStyle = textStyle
			}
			cols = append(cols, subStyle.Render(ansi.Truncate(sub, cells, "")))
		}
		for j, c := range cols {
			rows[j] += c + strings.Repeat(" ", max(0, span-lipgloss.Width(c)))
		}
	}
	rows = append(rows, currentRows...)
	for j := range rows {
		rows[j] = strings.TrimRight(rows[j], " ")
	}
	return rows
}

// huntLamp is a warning light under the stage track: unlit while no hunt runs, lit while one interrupts the stages.
func (s Snapshot) huntLamp() string {
	if s.phase == journal.PhaseHunt && s.hunt != nil {
		h := s.hunt
		what, paused := "the failure", "The lap waits"
		if h.cause != nil && h.cause.signal == machine.Crash {
			what = "the crash"
		}
		switch {
		case s.refine != nil:
			paused = fmt.Sprintf("Deepening round %d waits", s.refine.Round)
		case s.guard != nil && len(s.guard.Steps) > 0:
			paused = fmt.Sprintf("Lap %d waits at step %d", s.guard.Rotation, currentStep(s.guard))
		}
		return lamp.Render(fmt.Sprintf(" %-8s", fmt.Sprintf("HUNT %d", h.id))) + "  " +
			amber.Render(fmt.Sprintf("%s while I find which cores caused %s", paused, what))
	}
	text := "no hunt running"
	if s.hunts > 0 {
		text += fmt.Sprintf(", %s so far", plural(s.hunts, "hunt"))
	}
	return grey.Render(fmt.Sprintf(" %-8s", "HUNT")) + "  " + grey.Render(text)
}

func (s Snapshot) coreRows(width int) []string {
	cells := min(gaugeCells, max(width-rowText, 10))
	var out []string
	var ccds []int
	for _, c := range s.cores {
		if !slices.Contains(ccds, c.ccd) {
			ccds = append(ccds, c.ccd)
		}
	}
	slices.Sort(ccds)
	for i, ccd := range ccds {
		if i > 0 {
			out = append(out, "")
		}
		out = append(out, pad+grey.Render(fmt.Sprintf("%-*s%s", rowText, fmt.Sprintf("CCD %d", ccd), scale(cells))))
		for _, c := range s.cores {
			if c.ccd == ccd {
				out = append(out, c.row(cells))
			}
		}
	}
	return out
}

// scale labels every tenth count above the gauges.
func scale(cells int) string {
	line := []byte(strings.Repeat(" ", cells))
	last := -1
	for m := 10; m <= 50; m += 10 {
		label := fmt.Sprintf("-%d", m)
		end := (m-1)*cells/50 + 1
		if start := end - len(label); start > last {
			copy(line[start:end], label)
			last = end
		}
	}
	return string(line)
}

// row reads left to right as a sentence about the core: its number, the offset applied now and one word for where it
// stands, then the gauge. A core this test is judging is drawn bright, every other core grey; ► in the margin marks the
// cores that carry the load.
func (c coreView) row(cells int) string {
	marker, text, words := pad, grey, grey
	if c.loaded {
		marker = white.Render("►") + strings.Repeat(" ", margin-1)
	}
	if c.tested {
		text, words = white, textStyle
	}
	word := c.state()
	switch {
	case c.suspect:
		word, words = "suspect", amber
	case c.parked:
		word = "parked"
	}
	return marker + text.Render(fmt.Sprintf("%02d  %3d", c.id, c.applied)) + "  " + words.Render(fmt.Sprintf("%-19s", word)) + "  " + c.gauge(cells)
}

// gauge spans 0 to -50, one cell per count on a wide screen: filled up to the applied offset, a faint trace up to the
// offset the core goes back to when it sits shallower for now, and a red tick at its failure point.
func (c coreView) gauge(cells int) string {
	depth := func(offset int) int { return min(-offset, 50) * cells / 50 }
	fill, trace, track := grey.Render("▒"), grey.Render("░"), grey.Render("·")
	if c.tested {
		fill, trace, track = lit.Render("█"), lit.Render("░"), litTrack.Render("·")
	}
	tick := -1
	if c.fail != nil {
		tick = max(depth(*c.fail)-1, 0)
	}
	reached := c.tuned
	if c.phase == journal.PhaseSearch {
		reached = 0
		if c.pass != nil {
			reached = *c.pass
		}
	}
	applied, tuned := depth(c.applied), depth(reached)
	var b strings.Builder
	for i := range cells {
		switch {
		case i == tick:
			b.WriteString(red.Render("▌"))
		case i < applied:
			b.WriteString(fill)
		case i < tuned:
			b.WriteString(trace)
		default:
			b.WriteString(track)
		}
	}
	return b.String()
}

// scrollbar is the column left of a scrolling view, one glyph per row of its page: a thumb sized and placed by the
// share of the total lines the page shows, on a thin track. A view that fits its page gets no bar.
func scrollbar(page, total, top, scrolled int) []string {
	bar := make([]string, page)
	if scrolled == 0 {
		for i := range bar {
			bar[i] = " "
		}
		return bar
	}
	thumb := max(page*page/total, 1)
	start := top * (page - thumb) / scrolled
	for i := range bar {
		bar[i] = grey.Render("│")
		if i >= start && i < start+thumb {
			bar[i] = textStyle.Render("█")
		}
	}
	return bar
}

// state answers one question about a core: can it go deeper?
func (c coreView) state() string {
	switch c.phase {
	case journal.PhaseSearch:
		if c.checking {
			return "confirming"
		}
		return "searching"
	case journal.PhaseResident:
		return "can go deeper"
	case journal.PhaseDone, journal.PhaseGuard, journal.PhaseHunt, journal.PhaseRefine:
	}
	switch {
	case c.tuned == -50:
		return "maxed out"
	case c.fail != nil && *c.fail == c.tuned-1:
		return "limit found"
	}
	return "held back by others"
}

// historyLines lists what happened, newest first: the time into the session, a tag for the kind of event and the line.
func (s Snapshot) historyLines(width, rows int) []string {
	if !s.session {
		return nil
	}
	out := []string{grey.Render("What happened")}
	indent := strings.Repeat(" ", stampWidth+tagWidth+2)
	for i := len(s.history) - 1; i >= 0 && len(out) < rows; i-- {
		e := s.history[i]
		prefix := s.stamp(e) + toneStyle(e.tone).Render(fmt.Sprintf("%-*s", tagWidth, e.tag)) + "  "
		for j, l := range wrapStyled(e.sentence(), width-len(indent), textStyle) {
			if len(out) >= rows {
				break
			}
			if j == 0 {
				out = append(out, prefix+l)
			} else {
				out = append(out, indent+l)
			}
		}
	}
	return out
}

// logLines is the journal's own record, oldest first, one event per line.
func (s Snapshot) logLines(width int) []string {
	out := []string{grey.Render("Event log, as the journal records it"), ""}
	for _, e := range s.log {
		out = append(out, s.stamp(e)+textStyle.Render(ansi.Truncate(e.text, width-stampWidth, "...")))
	}
	return out
}

// stampWidth is the width of a stamp: the time into the session, right-aligned, and two spaces.
const stampWidth = 9

func (s Snapshot) stamp(e entry) string {
	return grey.Render(fmt.Sprintf("%*s  ", stampWidth-2, hm(e.at.Sub(s.start))))
}

// wrapStyled breaks text into lines of at most width cells, each rendered in style. A word longer than a line, such as
// an escaped diagnostic, is broken across lines rather than cut short.
func wrapStyled(text string, width int, style lipgloss.Style) []string {
	width = max(width, 10)
	var lines []string
	line := ""
	for word := range strings.FieldsSeq(text) {
		for ansi.StringWidth(word) > width {
			if line != "" {
				lines, line = append(lines, line), ""
			}
			lines = append(lines, ansi.Cut(word, 0, width))
			word = ansi.Cut(word, width, ansi.StringWidth(word))
		}
		switch {
		case word == "":
		case line == "":
			line = word
		case ansi.StringWidth(line)+1+ansi.StringWidth(word) <= width:
			line += " " + word
		default:
			lines = append(lines, line)
			line = word
		}
	}
	if line != "" || len(lines) == 0 {
		lines = append(lines, line)
	}
	for i, l := range lines {
		lines[i] = style.Render(l)
	}
	return lines
}

func fit(lines []string, width, h int) string {
	for len(lines) < h-1 {
		lines = append(lines, "")
	}
	lines = lines[:max(0, h-1)]
	for i, ln := range lines {
		lines[i] = ansi.Truncate(ln, width, "...")
	}
	return strings.Join(lines, "\n")
}

// ago says how long ago something happened, in words.
func ago(d time.Duration) string {
	d = max(d, 0)
	switch {
	case d < time.Minute:
		return "moments ago"
	case d < 2*time.Minute:
		return "a minute ago"
	case d < time.Hour:
		return fmt.Sprintf("%d minutes ago", int(d.Minutes()))
	}
	return hm(d) + " ago"
}

func age(d time.Duration) string {
	d = max(d, 0)
	switch {
	case d < time.Minute:
		return fmt.Sprintf("%ds", int(d.Seconds()))
	case d < time.Hour:
		return fmt.Sprintf("%dm%02ds", int(d.Minutes()), int(d.Seconds())%60)
	}
	return hm(d)
}

func clock(d time.Duration) string {
	d = max(d, 0)
	if d < time.Hour {
		return fmt.Sprintf("%dm%02ds", int(d.Minutes()), int(d.Seconds())%60)
	}
	return hm(d)
}

func hm(d time.Duration) string {
	d = max(d, 0)
	return fmt.Sprintf("%dh%02dm", int(d.Hours()), int(d.Minutes())%60)
}

func plural(n int, noun string) string {
	if n == 1 {
		return fmt.Sprintf("%d %s", n, noun)
	}
	if strings.HasSuffix(noun, "sh") {
		return fmt.Sprintf("%d %ses", n, noun)
	}
	return fmt.Sprintf("%d %ss", n, noun)
}
