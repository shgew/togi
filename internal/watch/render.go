package watch

import (
	"fmt"
	"slices"
	"strings"
	"time"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

// Console backgrounds use slots 0-7; bold would change foreground slots on TERM=linux.
var (
	plain     = lipgloss.NewStyle()
	textStyle = plain.Foreground(lipgloss.Color("7"))
	grey      = plain.Foreground(lipgloss.Color("8"))
	white     = plain.Foreground(lipgloss.Color("15"))
	lit       = plain.Foreground(lipgloss.Color("14"))
	amber     = plain.Foreground(lipgloss.Color("11"))
	green     = plain.Foreground(lipgloss.Color("10"))
	red       = plain.Foreground(lipgloss.Color("9"))
	magenta   = plain.Foreground(lipgloss.Color("13"))
	track     = plain.Foreground(lipgloss.Color("5"))
	chip      = white.Background(lipgloss.Color("5"))
	lamp      = plain.Foreground(lipgloss.Color("0")).Background(lipgloss.Color("3"))
)

func toneStyle(t tone) lipgloss.Style {
	switch t {
	case goodTone:
		return green
	case badTone:
		return red
	case warnTone, huntTone:
		return amber
	case comboTone:
		return magenta
	case plainTone:
		return textStyle
	}
	return textStyle
}

type View int

const (
	MainView View = iota
	HelpView
	LogView
)

// Scroll counts lines from the top; a negative value selects the end.
type Screen struct {
	View          View
	Keys          bool
	Scroll        int
	Width, Height int
}

type sizeClass int

const (
	compactLayout sizeClass = iota
	mediumLayout
	wideLayout
)

type rectangle struct {
	x, y, w, h int
}

type coreColumns struct {
	id, offset, state, gauge, cells, failure, note int
}

type layout struct {
	class                         sizeClass
	width, height, hint           int
	header, stage, say, last, now rectangle
	outcomes, context, history    rectangle
	body                          rectangle
	ccds                          []rectangle
	columns                       []coreColumns
	ccdIDs                        []int
}

func measure(s Snapshot, sc Screen) layout {
	p := layout{width: max(sc.Width-1, 0), height: max(sc.Height-1, 0)}
	p.hint = p.height - 1
	margin, gap := 1, 3
	stageY, sayY, nowY, ccdY := 1, 3, 5, 10
	nowH, outcomeY, outcomeH := 2, 8, 1
	p.class = compactLayout
	if sc.Width >= 200 && sc.Height >= 60 {
		p.class = wideLayout
		margin, gap = 2, 5
		stageY, sayY, nowY, ccdY = 2, 4, 7, 19
		nowH, outcomeY, outcomeH = 7, 15, 3
	} else if sc.Width >= 150 && sc.Height >= 40 {
		p.class = mediumLayout
		margin, gap = 2, 5
		stageY, sayY, nowY, ccdY = 2, 4, 7, 15
		nowH, outcomeY, outcomeH = 3, 11, 3
	}
	full := max(p.width-2*margin, 0)
	left := max((full-gap)/2, 0)
	rightX := margin + left + gap
	if p.class == wideLayout && full >= 235 {
		left, rightX, full = 115, 122, 235
	}
	p.header = rectangle{margin, 0, full, 1}
	p.stage = rectangle{margin, stageY, full, 1}
	p.say = rectangle{margin, sayY, full, 1}
	if p.class != compactLayout {
		p.say = rectangle{margin, sayY, left, 2}
	}
	p.last = rectangle{rightX, sayY, left, 2}
	p.now = rectangle{margin, nowY, full, nowH}
	p.outcomes = rectangle{margin, outcomeY, full, outcomeH}
	for _, c := range s.cores {
		if !slices.Contains(p.ccdIDs, c.ccd) {
			p.ccdIDs = append(p.ccdIDs, c.ccd)
		}
	}
	slices.Sort(p.ccdIDs)
	side := left >= 50
	lowY := ccdY
	for i, id := range p.ccdIDs {
		rows := 0
		for _, c := range s.cores {
			if c.ccd == id {
				rows++
			}
		}
		x, y, w := margin, lowY, full
		if side {
			x, y, w = margin+(i%2)*(left+gap), ccdY, left
			if i >= 2 {
				y = lowY
			}
		}
		height := min(rows+1, max(p.hint-y-1, 0))
		if !side {
			share := max((p.hint-ccdY-3)/max(len(p.ccdIDs), 1), 1)
			height = min(height, share)
		}
		r := rectangle{x, y, w, height}
		p.ccds = append(p.ccds, r)
		col := coreColumns{2, 5, 9, 18, 25, 44, 50}
		if p.class == wideLayout {
			col = coreColumns{2, 6, 11, 21, 50, 73, 81}
		} else if p.class == mediumLayout || !side {
			col = coreColumns{2, 6, 11, 21, 25, 48, 56}
		}
		col.cells = min(col.cells, max(w-col.gauge-1, 0))
		p.columns = append(p.columns, col)
		lowY = max(lowY, y+r.h+1)
	}
	if len(p.ccdIDs) == 0 {
		lowY = ccdY
	}
	p.context = rectangle{margin, lowY, left, max(p.hint-lowY-1, 0)}
	p.history = rectangle{rightX, lowY, left, p.context.h}
	if !side {
		p.context.w = full
		p.history = rectangle{margin, lowY + p.context.h, full, 0}
	}
	p.body = rectangle{margin, sayY, full, max(p.hint-sayY-1, 0)}
	return p
}

type canvas struct {
	lines []string
	width int
}

func newCanvas(p layout) canvas {
	return canvas{make([]string, p.height), p.width}
}

func (c *canvas) put(r rectangle, x, y int, text string) {
	c.place(r, x, y, text, true)
}

func (c *canvas) putCells(r rectangle, x, y int, text string) {
	c.place(r, x, y, text, false)
}

func (c *canvas) place(r rectangle, x, y int, text string, words bool) {
	if x < 0 || y < 0 || x >= r.w || y >= r.h || r.y+y < 0 || r.y+y >= len(c.lines) {
		return
	}
	x += r.x
	if x < 0 || x >= c.width {
		return
	}
	width := min(r.w-(x-r.x), c.width-x)
	text = consoleText(text)
	if words {
		text = trimWords(text, width)
	} else {
		text = ansi.Truncate(text, width, "")
	}
	line := c.lines[r.y+y]
	before := ansi.Cut(line, 0, x)
	if n := x - ansi.StringWidth(before); n > 0 {
		before += strings.Repeat(" ", n)
	}
	c.lines[r.y+y] = before + text + ansi.Cut(line, x+ansi.StringWidth(text), c.width)
}

func (c *canvas) rows(r rectangle, lines []string) {
	lines = boundedRows(lines, r.h)
	for y, line := range lines {
		c.put(r, 0, y, line)
	}
}

func boundedRows(lines []string, height int) []string {
	if height <= 0 {
		return nil
	}
	if len(lines) <= height {
		return lines
	}
	out := make([]string, height)
	copy(out, lines[:height-1])
	out[height-1] = grey.Render(fmt.Sprintf("+%d more", len(lines)-height+1))
	return out
}

func rule(width int, title, right string) string {
	width = max(width, 0)
	title = ansi.Truncate(title, width, "")
	tw, rw := ansi.StringWidth(title), ansi.StringWidth(right)
	if rw == 0 || tw+rw+2 > width {
		return title + " " + track.Render(strings.Repeat("─", max(width-tw-1, 0)))
	}
	return title + " " + track.Render(strings.Repeat("─", width-tw-rw-2)) + " " + right
}

func Render(s Snapshot, w, h int, now time.Time) string {
	return strings.Join(RenderView(s, Screen{View: MainView, Width: w, Height: h}, now).Lines, "\n")
}

func RenderView(s Snapshot, sc Screen, now time.Time) Drawn {
	p := measure(s, sc)
	c := newCanvas(p)
	until := s.quietUntil()
	if !until.IsZero() && now.Before(until) {
		now = s.trial.started
	} else {
		until = time.Time{}
	}
	header := s.header(now)
	if p.class == compactLayout && !until.IsZero() {
		header = grey.Render("togi") + "  " + white.Render(now.Format("15:04:05")) + "  " + amber.Render("paused until "+until.Format("15:04")) +
			grey.Render("  session ") + textStyle.Render(hm(now.Sub(s.start))) +
			grey.Render(fmt.Sprintf("  %d failures · %d crashes", s.failures, s.crashes))
	}
	c.put(p.header, 0, 0, header)
	c.put(p.stage, 0, 0, s.stageLine(p.class, sc.View != MainView, now))
	scroll := 0
	switch sc.View {
	case HelpView:
		var body []string
		body, scroll = renderHelpBody(p.body.w, p.body.h, sc.Scroll)
		c.rows(p.body, body)
	case LogView:
		var body []string
		body, scroll = renderLogBody(s, p.body.w, p.body.h, sc.Scroll)
		c.rows(p.body, body)
	case MainView:
		st := s.story(now)
		say := p.say
		if !s.session || s.problem != nil {
			say = p.body
		}
		lines := make([]string, 0, len(st.paragraphs)+1)
		for i, line := range st.paragraphs {
			prefix := ""
			if i == 0 && p.class != compactLayout {
				prefix = narrativeStyle(st.tone).Render(st.headline) + "   "
			}
			for _, part := range wrapStyled(prefix+line, max(say.w-3, 1), plain) {
				lines = append(lines, narrativeStyle(st.tone).Render("▌")+"  "+part)
			}
		}
		if len(lines) > say.h {
			lines = lines[:say.h]
		}
		c.rows(say, lines)
		if s.session && s.problem == nil {
			if p.class != compactLayout {
				c.rows(p.last, s.lastLines())
			}
			drawNow(&c, p, s, now)
			drawOutcomes(&c, p, s)
			for i, r := range p.ccds {
				drawCCD(&c, r, p.columns[i], p.ccdIDs[i], s)
			}
			c.rows(p.context, s.contextLines(p, now))
			c.rows(p.history, s.historyPanel(p.history.w))
		}
	}
	if sc.Keys {
		c.put(rectangle{p.header.x, p.hint, p.header.w, 1}, 0, 0, keyHints(sc.View))
	}
	return Drawn{Lines: fit(c.lines, p.width, sc.Height), Scroll: scroll, Until: until}
}

func narrativeStyle(t tone) lipgloss.Style {
	if t == plainTone {
		return lit
	}
	return toneStyle(t)
}

func (s Snapshot) quietUntil() time.Time {
	if s.problem == nil && s.session && s.deadEnd == nil && s.stopped == nil && s.trial != nil && s.trial.hasStarted && s.trial.regime == machine.R6 && s.trial.duration > 0 {
		return s.trial.started.Add(s.trial.duration)
	}
	return time.Time{}
}

func (s Snapshot) header(now time.Time) string {
	out := grey.Render("togi") + "   " + white.Render(now.Format("15:04:05"))
	if !s.session {
		return out + grey.Render("   no session yet")
	}
	end := now
	if s.deadEnd != nil {
		end = s.deadEnd.at
	} else if s.stopped != nil {
		end = s.stopped.at
	}
	out += grey.Render("   session ") + textStyle.Render(hm(end.Sub(s.start))) + "   " + textStyle.Render(fmt.Sprint(s.failures)) + grey.Render(" failures · ") + textStyle.Render(fmt.Sprint(s.crashes)) + grey.Render(" crashes")
	if s.lastFailure != nil {
		out += grey.Render("   last observed failure ") + textStyle.Render(s.lastFailure.Format("15:04"))
		if s.quietUntil().IsZero() {
			out += grey.Render(", " + ago(now.Sub(*s.lastFailure)))
		}
	}
	if until := s.quietUntil(); !until.IsZero() && now.Before(until) {
		out += "   " + amber.Render("screen paused until "+until.Format("15:04"))
	}
	return out
}

func (s Snapshot) stageLine(class sizeClass, summary bool, now time.Time) string {
	if !s.session || s.problem != nil {
		return ""
	}
	found, room := 0, 0
	for _, c := range s.cores {
		if c.solo != nil {
			found++
		}
		if c.state == coreHasRoom {
			room++
		}
	}
	short := class == compactLayout
	names := []string{"SOLO LIMITS", "CYCLES", "DEEPEN", "CLEAN CYCLES"}
	join := "  " + track.Render("───") + "  "
	if short {
		names = []string{"SOLO", "CYCLE", "DEEPEN", "CLEAN"}
		join = " " + track.Render("─") + " "
	}
	solo := s.soloStage(names[0], found, short)
	cycle := s.cycleStage(names[1], short)
	parts := []string{solo, cycle}
	if h := s.hunt; h != nil {
		hunt := amber.Render("► ") + lamp.Render(fmt.Sprintf(" HUNT %d ", h.id))
		if t := s.trial; t != nil {
			if t.probe != nil {
				hunt += amber.Render(fmt.Sprintf("  member probes · core %02d at %d", t.probe.Core, t.probe.Offset))
			} else if t.huntParts > 0 {
				hunt += amber.Render(fmt.Sprintf("  parts · part %d of %d", t.huntPart, t.huntParts))
			}
		}
		parts = append(parts, hunt)
	}
	deep := grey.Render("○ " + names[2])
	if s.phase == journal.PhaseDeepening && s.hunt == nil {
		deep = lit.Render("► ") + white.Render(names[2])
		if s.deepen != nil {
			deep += textStyle.Render(fmt.Sprintf("  round %d", s.deepen.round))
		}
	} else if !short && s.hunt == nil && room > 0 {
		deep += grey.Render(fmt.Sprintf("  after a passed full cycle · %d cores have room", room))
	}
	parts = append(parts, deep, grey.Render(fmt.Sprintf("%s %d", names[3], s.cleanCycles)))
	if !short {
		parts[len(parts)-1] += track.Render("  · repeats until stopped")
	}
	if summary && s.trial != nil && s.trial.hasStarted {
		t := s.trial
		if until := s.quietUntil(); !until.IsZero() && now.Before(until) {
			parts = append(parts, grey.Render("screen paused until "+until.Format("15:04")))
		} else {
			parts = append(parts, white.Render(clock(max(t.duration-now.Sub(t.started), 0))+" left"))
		}
	}
	return strings.Join(parts, join)
}

func (s Snapshot) soloStage(name string, found int, short bool) string {
	if found == len(s.cores) && found > 0 {
		solo := green.Render("■ ") + textStyle.Render(name)
		if !short {
			solo += grey.Render(fmt.Sprintf("  %d/%d", found, len(s.cores)))
		}
		return solo
	}
	solo := lit.Render("► ") + white.Render(name) + textStyle.Render(fmt.Sprintf("  %d of %d found", found, len(s.cores)))
	if s.trial != nil && s.trial.condition == machine.Alone && !short {
		verb := "searching"
		if c := s.core(s.trial.core); c != nil && c.confirm != nil {
			verb = "confirming"
		}
		solo += textStyle.Render(fmt.Sprintf(" · core %02d %s %d", s.trial.core, verb, s.trial.offset))
	}
	return solo
}

func (s Snapshot) cycleStage(name string, short bool) string {
	g := s.cycle
	if g == nil {
		return grey.Render("○ " + name)
	}
	name = fmt.Sprintf("CYCLE %d", g.number)
	var cycle string
	switch {
	case g.paused || s.hunt != nil:
		cycle = amber.Render("○ ") + textStyle.Render(name)
		if short {
			cycle += amber.Render(fmt.Sprintf(" paused at %d/%d", min(g.current+1, len(g.steps)), len(g.steps)))
		} else {
			cycle += amber.Render(fmt.Sprintf("  paused at step %d of %d", min(g.current+1, len(g.steps)), len(g.steps)))
		}
	case s.phase == journal.PhaseChecking:
		cycle = lit.Render("► ") + white.Render(name)
		if !short {
			cycle += textStyle.Render(fmt.Sprintf("  step %d of %d", min(g.current+1, len(g.steps)), len(g.steps)))
		}
	default:
		cycle = green.Render("■ ") + textStyle.Render(name)
	}
	if t := s.trial; t != nil && t.parts > 0 && !short {
		cycle += textStyle.Render(fmt.Sprintf(" · part %d of %d", t.part, t.parts))
		if !g.paused {
			cycle += textStyle.Render(fmt.Sprintf(" · trial %d of %d", t.index, t.of))
		}
	}
	return cycle
}

func keyHints(view View) string {
	type key struct {
		key, label      string
		active, current bool
	}
	keys := []key{{"?", "help", true, view == HelpView}, {"l", "log", true, view == LogView}, {"esc", "back", view != MainView, false}, {"q", "close view", true, false}}
	parts := make([]string, 0, len(keys))
	for _, k := range keys {
		style, label := chip, grey
		if k.current {
			style = plain.Foreground(lipgloss.Color("0")).Background(lipgloss.Color("7"))
		} else if !k.active {
			style, label = track, track
		}
		parts = append(parts, style.Render(" "+k.key+" ")+" "+label.Render(k.label))
	}
	return strings.Join(parts, "   ")
}

func drawRestingNow(c *canvas, p layout, s Snapshot) {
	r := p.now
	title, style := "BETWEEN TRIALS", grey
	var lines []string
	switch {
	case s.deadEnd != nil:
		title, style = "DEAD END", red
		lines = append(lines, strings.ReplaceAll(vtText(string(s.deadEnd.condition)), "_", " ")+": "+vtText(s.deadEnd.detail))
	case s.stopped != nil:
		title = "STOPPED"
		lines = append(lines, s.stopped.at.Format("15:04:05")+" · "+stopWords(s.stopped.reason))
		if s.stopped.saved {
			lines = append(lines, "Rows show the saved profile, not applied now.")
		}
	case s.recover != nil:
		title, style = "RECOVERED FROM A CRASH", amber
		what := "no trial was in flight"
		if s.recover.trial != nil {
			what = trialDescription(*s.recover.trial)
		}
		lines = append(lines, what+" · detected "+s.recover.bootAt.Format("15:04:05"), s.nextLine())
	default:
		if s.last != nil {
			lines = append(lines, lastDescription(*s.last))
		}
		lines = append(lines, s.nextLine())
	}
	if p.class == compactLayout {
		c.put(r, 0, 0, style.Render(title)+"  "+textStyle.Render(strings.Join(lines, " · ")))
		if len(lines) > 1 {
			c.put(r, 0, 1, textStyle.Render(lines[len(lines)-1]))
		}
		return
	}
	c.put(r, 0, 0, rule(r.w, style.Render(title), ""))
	body := rectangle{r.x, r.y + 1, r.w, r.h - 1}
	c.rows(body, wrapStyled(strings.Join(lines, "\n"), r.w, textStyle))
}

func drawNow(c *canvas, p layout, s Snapshot, now time.Time) {
	r := p.now
	if s.deadEnd != nil || s.stopped != nil || s.recover != nil || s.trial == nil {
		drawRestingNow(c, p, s)
		return
	}
	t := s.trial
	op := s.operation(*t)
	description := chip.Render(" "+string(t.regime)+" ") + "  " + white.Render(loadWords(t.regime)) + "  " + textStyle.Render(trialWhere(*t)) + "   " + grey.Render(workloadDisplay(t.workload))
	if workers := len(t.cores) * t.workload.Threads; workers > 0 {
		suffix := "s"
		if workers == 1 {
			suffix = ""
		}
		description += grey.Render(fmt.Sprintf(" · %d worker%s", workers, suffix))
	}
	quiet := t.hasStarted && t.regime == machine.R6 && now.Before(t.started.Add(t.duration))
	if quiet {
		description = chip.Render(" R6 ") + "  " + white.Render("idle + bursts") + "  " + textStyle.Render("on "+coresText(t.cores, len(s.cores))) + "   " + grey.Render("short wake-ups, no workers in between")
	}
	if p.class == wideLayout || p.class == mediumLayout {
		c.put(r, 0, 0, rule(r.w, grey.Render("NOW"), grey.Render("trial "+vtText(t.id))))
	}
	descY := 0
	if p.class != compactLayout {
		descY = 1
	}
	opWidth := ansi.StringWidth(op)
	c.put(r, 0, descY, trimWords(description, max(r.w-opWidth-3, 0)))
	c.put(r, max(r.w-opWidth, 0), descY, op)
	if !t.hasStarted {
		c.put(r, 0, r.h-1, grey.Render("preparing · workload has not started yet · planned "+clock(t.duration)))
		return
	}
	elapsed := min(max(now.Sub(t.started), 0), max(t.duration, 0))
	remaining := max(t.duration-elapsed, 0)
	frac := 0.0
	if t.duration > 0 {
		frac = float64(elapsed) / float64(t.duration)
	}
	if p.class == wideLayout {
		cells := max(r.w-23, 0)
		digits, style := clock(remaining), white
		if quiet {
			digits, style = t.started.Add(t.duration).Format("15:04"), grey
		}
		for y := 3; y <= 5; y++ {
			glyph := "█"
			if y == 5 {
				glyph = "▀"
			}
			line := progressBar(cells, frac, glyph)
			if quiet {
				if y != 5 {
					glyph = "░"
				}
				line = track.Render(strings.Repeat(glyph, cells))
			}
			c.putCells(r, 0, y, line)
		}
		for y, line := range bigDigits(digits) {
			c.putCells(r, r.w-ansi.StringWidth(line), 3+y, style.Render(line))
		}
		if quiet {
			msg := "  screen paused until " + digits + " so it cannot wake the idle cores  "
			c.put(r, max((cells-ansi.StringWidth(msg))/2, 0), 4, textStyle.Render(msg))
			c.put(r, 0, 6, grey.Render("started ")+textStyle.Render(t.started.Format("15:04"))+grey.Render(" · "+clock(t.duration)))
			c.put(r, r.w-7, 6, grey.Render("ends at"))
		} else {
			c.put(r, 0, 6, textStyle.Render(clock(elapsed))+grey.Render(" of "+clock(t.duration)))
			trials := requirementLine(*t, false)
			c.put(r, max(cells-ansi.StringWidth(trials), 0), 6, trials)
			c.put(r, r.w-4, 6, grey.Render("left"))
		}
	} else {
		y := r.h - 1
		label := white.Render(clock(remaining)) + grey.Render(" left") + "  " + requirementLine(*t, true)
		if quiet {
			label = grey.Render("screen paused until ") + textStyle.Render(t.started.Add(t.duration).Format("15:04")) + grey.Render(" · ends at")
		}
		cells := max(r.w-ansi.StringWidth(label)-2, 0)
		line := progressBar(cells, frac, "█")
		if quiet {
			line = track.Render(strings.Repeat("░", cells))
		}
		c.putCells(r, 0, y, line+"  "+label)
	}
}

func requirementLine(t trialView, compact bool) string {
	if t.of <= 0 {
		return ""
	}
	label := fmt.Sprintf("%d passed · trial %d of %d  ", t.passed, t.index, t.of)
	if compact {
		label = fmt.Sprintf("%d/%d ", t.passed, t.of)
	}
	var b strings.Builder
	b.WriteString(textStyle.Render(label))
	for i := range t.of {
		switch {
		case i < t.passed:
			b.WriteString(green.Render("■"))
		case i == t.index-1:
			b.WriteString(lit.Render("►"))
		default:
			b.WriteString(grey.Render("·"))
		}
		if i+1 < t.of {
			b.WriteByte(' ')
		}
	}
	return b.String()
}

func progressBar(cells int, frac float64, glyph string) string {
	cells = max(cells, 0)
	n := int(float64(cells) * min(max(frac, 0), 1))
	return lit.Render(strings.Repeat(glyph, n)) + track.Render(strings.Repeat(glyph, cells-n))
}

var digitGlyphs = [11][3]string{
	{"█▀█", "█ █", "▀▀▀"}, {"▀█ ", " █ ", "▀▀▀"}, {"▀▀█", "█▀▀", "▀▀▀"},
	{"▀▀█", " ▀█", "▀▀▀"}, {"█ █", "▀▀█", "  ▀"}, {"█▀▀", "▀▀█", "▀▀▀"},
	{"█▀▀", "█▀█", "▀▀▀"}, {"▀▀█", "  █", "  ▀"}, {"█▀█", "█▀█", "▀▀▀"},
	{"█▀█", "▀▀█", "▀▀▀"}, {" ", "▀", "▀"},
}

func bigDigits(text string) [3]string {
	var rows [3]strings.Builder
	for i, ch := range text {
		n := int(ch - '0')
		if ch == ':' {
			n = 10
		}
		if n < 0 || n >= len(digitGlyphs) {
			continue
		}
		for y := range rows {
			if i > 0 {
				rows[y].WriteByte(' ')
			}
			rows[y].WriteString(digitGlyphs[n][y])
		}
	}
	return [3]string{rows[0].String(), rows[1].String(), rows[2].String()}
}

func drawOutcomes(c *canvas, p layout, s Snapshot) {
	if s.trial == nil || s.stopped != nil || s.deadEnd != nil || s.recover != nil {
		return
	}
	rows := s.outcomeRows()
	if p.class == compactLayout {
		width := max((p.outcomes.w-4)/max(len(rows), 1), 0)
		for i, o := range rows {
			label := strings.TrimPrefix(o.label, "if ")
			label = strings.ReplaceAll(label, "a core is named", "core named")
			label = strings.ReplaceAll(label, "none is named", "none named")
			label = strings.ReplaceAll(label, "it passes", "pass")
			label = strings.ReplaceAll(label, "it fails", "fail")
			line := o.style.Render(label) + " " + textStyle.Render(o.compact)
			c.put(p.outcomes, i*(width+2), 0, trimWords(line, width))
		}
		return
	}
	for i, o := range rows {
		c.put(p.outcomes, 0, i, o.style.Render(fmt.Sprintf("%-21s", o.label))+trimWords(textStyle.Render(o.text), max(p.outcomes.w-21, 0)))
	}
}

func drawCCD(c *canvas, r rectangle, col coreColumns, id int, s Snapshot) {
	c.put(r, 0, 0, white.Render(fmt.Sprintf("CCD %d", id)))
	c.put(r, col.gauge-1, 0, grey.Render("0"))
	if col.cells == 50 {
		for n := 10; n <= 50; n += 10 {
			c.put(r, col.gauge+n-3, 0, grey.Render(fmt.Sprintf("-%d", n)))
		}
	} else {
		c.put(r, col.gauge+2, 0, track.Render("2 counts/cell"))
		c.put(r, col.gauge+col.cells-3, 0, grey.Render("-50"))
	}
	c.put(r, col.failure-1, 0, grey.Render("fail pt"))
	c.put(r, col.note, 0, grey.Render(s.ccdRole(id)))
	var cores []coreView
	for _, core := range s.cores {
		if core.ccd == id {
			cores = append(cores, core)
		}
	}
	shown := min(len(cores), max(r.h-1, 0))
	if len(cores) > shown && shown > 0 {
		shown--
		c.put(r, 0, r.h-1, grey.Render(fmt.Sprintf("+%d more", len(cores)-shown)))
	}
	for i, core := range cores[:shown] {
		if s.stopped != nil && s.stopped.saved {
			core.applied = core.profile
		}
		y := i + 1
		style := textStyle
		if core.judged {
			style = white
		}
		if core.loaded {
			c.put(r, 0, y, lit.Render("►"))
		}
		c.put(r, col.id, y, style.Render(fmt.Sprintf("%02d", core.id)))
		c.put(r, col.offset, y, style.Render(fmt.Sprintf("%3d", core.applied)))
		stateStyle := textStyle
		switch core.state {
		case coreParked, coreWaiting:
			stateStyle = grey
		case coreProbe:
			stateStyle = amber
		case coreSearch, coreConfirm, coreFound, coreAtLimit, coreHasRoom, coreSuspect, coreMember:
		}
		c.put(r, col.state, y, stateStyle.Render(core.stateWord()))
		c.putCells(r, col.gauge, y, core.gauge(col.cells))
		if core.fail != nil {
			c.put(r, col.failure, y, style.Render(fmt.Sprintf("%5d", *core.fail)))
		}
		note := core.noteLine(max(r.w-col.note, 0))
		c.put(r, col.note, y, trimWords(note, max(r.w-col.note, 0)))
	}
}

func (c coreView) stateWord() string {
	switch c.state {
	case coreWaiting:
		return "WAITING"
	case coreSearch:
		return "SEARCH"
	case coreConfirm:
		return "CONFIRM"
	case coreFound:
		return "FOUND"
	case coreAtLimit:
		return "AT LIMIT"
	case coreHasRoom:
		return "HAS ROOM"
	case coreSuspect:
		return "SUSPECT"
	case coreMember:
		return "MEMBER"
	case coreProbe:
		return "PROBE"
	case coreParked:
		return "PARKED"
	}
	return "WAITING"
}

func (c coreView) gauge(cells int) string {
	cells = max(cells, 0)
	if cells == 0 {
		return ""
	}
	back := 0
	for _, point := range []*int{c.solo, c.pass, c.returnsTo} {
		if point != nil && *point < back {
			back = *point
		}
	}
	held := 0
	if c.holder != nil && c.holder.combination > 0 {
		held = back
		if c.solo != nil {
			held = *c.solo
		}
	}
	var b strings.Builder
	b.Grow(cells*3 + 64)
	run, count, runStyle := "", 0, plain
	flush := func() {
		if count > 0 {
			b.WriteString(runStyle.Render(strings.Repeat(run, count)))
		}
	}
	for i := range cells {
		lo, hi := i*50/cells+1, (i+1)*50/cells
		glyph, style := " ", plain
		switch {
		case c.fail != nil && -*c.fail >= lo && -*c.fail <= hi:
			glyph, style = "█", red
		case slices.ContainsFunc(c.groupFails, func(mark int) bool { return -mark >= lo && -mark <= hi }):
			glyph, style = "▀", red
		case -c.applied >= lo:
			glyph, style = "▄", grey
			if c.judged {
				style = white
			}
		case -held >= lo:
			glyph, style = "░", magenta
		case -back >= lo:
			glyph, style = "·", grey
		}
		if glyph != run {
			flush()
			run, count, runStyle = glyph, 0, style
		}
		count++
	}
	flush()
	return b.String()
}

func (c coreView) noteLine(width int) string {
	switch {
	case c.state == coreProbe && len(c.groupFails) > 0:
		prefix := "group failed at "
		if width < 34 {
			prefix = ""
		}
		return textStyle.Render(prefix + offsetList(c.groupFails))
	case c.returnsTo != nil:
		if width < 12 {
			return grey.Render(fmt.Sprintf("%4d", *c.returnsTo))
		}
		return grey.Render(fmt.Sprintf("returns to %d", *c.returnsTo))
	case c.confirm != nil:
		n := c.confirm
		if width < 34 {
			return textStyle.Render(fmt.Sprintf("L%d/%d H%d/%d", n.light, n.needed, n.heavy, n.needed))
		}
		return textStyle.Render(fmt.Sprintf("%d: light %d/%d, heavy %d/%d", n.offset, n.light, n.needed, n.heavy, n.needed))
	case c.holder != nil && c.holder.combination > 0:
		label := fmt.Sprintf("C%d", c.holder.combination)
		if width >= 34 {
			label = "held by " + label
		}
		out := magenta.Render(label)
		if c.solo != nil && width >= 12 {
			out += grey.Render(fmt.Sprintf(" · solo %d", *c.solo))
		}
		return out
	case c.gaveBack > 0:
		out := fmt.Sprintf("C%d", c.gaveBack)
		if width >= 34 {
			out = "backed off after " + out
		}
		if c.solo != nil && width >= 12 {
			out += fmt.Sprintf(" · solo %d", *c.solo)
		}
		return grey.Render(out)
	case c.state == coreFound && c.solo != nil:
		if width < 12 {
			return grey.Render(fmt.Sprint(*c.solo))
		}
		return grey.Render(fmt.Sprintf("solo %d", *c.solo))
	case c.state == coreSearch:
		if width < 12 && c.next != nil {
			return grey.Render(fmt.Sprintf("→ %d", *c.next))
		}
		var out []string
		if c.pass != nil {
			label := "passed "
			if width < 34 {
				label = "p "
			}
			out = append(out, fmt.Sprintf("%s%d", label, *c.pass))
		}
		if c.next != nil {
			label := "next "
			if width < 34 {
				label = "n "
			}
			out = append(out, fmt.Sprintf("%s%d", label, *c.next))
		}
		return grey.Render(strings.Join(out, " · "))
	case c.queued != "":
		return grey.Render(vtText(c.queued))
	}
	return ""
}

func (s Snapshot) ccdRole(id int) string {
	loaded, judged, parked, total := 0, 0, 0, 0
	atZero := true
	for _, c := range s.cores {
		if c.ccd != id {
			continue
		}
		total++
		offset := c.applied
		if s.stopped != nil && s.stopped.saved {
			offset = c.profile
		}
		atZero = atZero && offset == 0
		if c.loaded {
			loaded++
		}
		if c.judged {
			judged++
		}
		if c.state == coreParked {
			parked++
		}
	}
	switch {
	case total > 0 && parked == total && loaded == total:
		return "parked, still under load"
	case total > 0 && parked == total:
		return "parked, idle"
	case total > 0 && loaded == total:
		return "all under load"
	case total > 0 && judged == total && loaded == 0:
		return "all idle, all judged"
	case loaded == 0:
		if atZero {
			return "waiting at 0"
		}
		return "not under load"
	case loaded == 1:
		return "one core under load at a time"
	}
	return fmt.Sprintf("%d under load · %d judged", loaded, judged)
}

func (s Snapshot) historyPanel(width int) []string {
	if width <= 0 {
		return nil
	}
	right := "newest first · l: every event"
	if width < 100 {
		right = "l: all"
	}
	out := []string{rule(width, grey.Render("WHAT HAPPENED"), grey.Render(right)), ""}
	for _, e := range s.history {
		out = append(out, grey.Render(e.at.Format("15:04"))+"  "+toneStyle(e.tone).Render(fmt.Sprintf("%-8s", e.tag))+" "+trimWords(textStyle.Render(vtText(e.sentence())), max(width-16, 0)))
	}
	return out
}

func (s Snapshot) contextLines(p layout, now time.Time) []string {
	w := p.context.w
	if s.hunt != nil {
		return s.huntLines(w, p.class, now)
	}
	if len(s.turns) > 0 {
		return s.turnLines(w, p.class)
	}
	if s.phase == journal.PhaseDeepening && s.deepen != nil {
		return s.deepenLines(w)
	}
	var out []string
	if s.cycle != nil {
		out = s.cycleLines(w, p.class)
	}
	if len(s.combos) > 0 {
		if len(out) > 0 {
			out = append(out, "")
		}
		out = append(out, s.combinationLines(w, p.class)...)
	}
	return out
}

func (s Snapshot) cycleLines(width int, class sizeClass) []string {
	g := s.cycle
	done := 0
	for _, step := range g.steps {
		if step.done {
			done++
		}
	}
	out := []string{rule(width, grey.Render(fmt.Sprintf("CYCLE %d", g.number)), grey.Render(fmt.Sprintf("%d steps · %d done", len(g.steps), done))), ""}
	if class == wideLayout {
		out = append(out, tableRow(width, []int{2, 6, 28, 52, 78, 93}, []string{"#", "kind", "where", "workload", "schedule", "result"}, grey))
	}
	for i, step := range g.steps {
		marker, style := "○", grey
		result := ""
		if step.done {
			marker, style, result = "■", textStyle, "done"
		} else if i == g.current {
			marker, style = "►", white
			if g.paused {
				marker, result = "○", "paused"
			}
		}
		kind := string(step.regime) + " " + strings.TrimSuffix(loadWords(step.regime), " load")
		where, schedule := stepSchedule(step)
		if len(step.hunts) > 0 {
			result += " · hunts " + numberList(step.hunts)
		}
		if class == wideLayout {
			out = append(out, stageMarker(marker)+tableRow(width-1, []int{1, 5, 27, 51, 77, 92}, []string{fmt.Sprintf("%2d", i+1), kind, where, workloadDisplay(step.workload), schedule, result}, style))
		} else {
			out = append(out, trimWords(stageMarker(marker)+" "+style.Render(fmt.Sprintf("%2d %-17s", i+1, kind))+" "+grey.Render(schedule)+" "+style.Render(result), width))
		}
		if i != g.current || step.done {
			continue
		}
		out = append(out, s.cyclePartLines(step, width, class)...)
	}
	return out
}

func (s Snapshot) cyclePartLines(step cycleStep, width int, class sizeClass) []string {
	var out []string
	for j, part := range step.parts {
		if len(step.parts) == 1 && !part.recordOnly {
			continue
		}
		marker := " "
		if part.running {
			marker = "►"
		}
		label := "part"
		if step.regime == machine.R7 {
			label = "partial"
			if part.full {
				label = "full"
			}
			if part.ccd < 0 {
				label += " CCD 0+1"
			} else {
				label += fmt.Sprintf(" CCD %d", part.ccd)
			}
		}
		where := coreIDs(part.cores)
		if part.recordOnly {
			where += " · record only"
		}
		result := fmt.Sprintf("%d/%d passed", part.passed, part.short+part.long)
		if part.recordOnly {
			result = fmt.Sprintf("recorded: %d passed", part.passed)
			if part.failed > 0 {
				result += fmt.Sprintf(", %d failed", part.failed)
			}
		} else if part.running && s.trial != nil {
			result = fmt.Sprintf("trial %d of %d", s.trial.index, s.trial.of)
		}
		tree := "├"
		if j == len(step.parts)-1 {
			tree = "└"
		}
		if class == wideLayout {
			out = append(out, stageMarker(marker)+tableRow(width-1, []int{5, 27, 77, 92}, []string{tree + " " + label, where, partSchedule(part), result}, textStyle))
		} else {
			out = append(out, trimWords(stageMarker(marker)+"   "+textStyle.Render(label+" "+where)+" "+grey.Render(result), width))
		}
	}
	return out
}

func tableRow(width int, columns []int, values []string, style lipgloss.Style) string {
	var b strings.Builder
	x := 0
	for i, at := range columns {
		if at >= width || i >= len(values) {
			break
		}
		b.WriteString(strings.Repeat(" ", max(at-x, 0)))
		next := width
		if i+1 < len(columns) {
			next = columns[i+1] - 1
		}
		value := trimWords(style.Render(values[i]), max(next-at, 0))
		b.WriteString(value)
		x = at + ansi.StringWidth(value)
	}
	return b.String()
}

func stageMarker(mark string) string {
	switch mark {
	case "■":
		return green.Render(mark)
	case "►":
		return lit.Render(mark)
	}
	return track.Render(mark)
}

func partSchedule(p cyclePart) string {
	var out []string
	if p.short > 0 {
		out = append(out, fmt.Sprintf("%d x %s", p.short, shortDuration(p.shortLen)))
	}
	if p.long > 0 {
		if p.long == 1 {
			out = append(out, shortDuration(p.longLen))
		} else {
			out = append(out, fmt.Sprintf("%d x %s", p.long, shortDuration(p.longLen)))
		}
	}
	return strings.Join(out, " + ")
}

func stepSchedule(step cycleStep) (string, string) {
	if step.regime == machine.R7 {
		return fmt.Sprintf("%d CCDs · %d parts", countCCDs(step.parts), len(step.parts)), fmt.Sprintf("%d parts", len(step.parts))
	}
	where := "one core at a time"
	if step.regime == machine.R6 {
		where = "all cores idle"
	}
	if len(step.parts) == 1 {
		return where, partSchedule(step.parts[0])
	}
	if len(step.parts) > 0 {
		return where, fmt.Sprintf("%d x %s", len(step.parts), shortDuration(step.parts[0].shortLen))
	}
	return where, ""
}

func countCCDs(parts []cyclePart) int {
	var ids []int
	for _, part := range parts {
		if part.ccd >= 0 && !slices.Contains(ids, part.ccd) {
			ids = append(ids, part.ccd)
		}
	}
	return len(ids)
}

func (s Snapshot) combinationLines(width int, class sizeClass) []string {
	out := []string{rule(width, grey.Render("COMBINATIONS"), grey.Render("offsets that failed together")), ""}
	var ids []int
	for _, combo := range s.combos {
		for _, member := range combo.members {
			if !slices.Contains(ids, member.Core) {
				ids = append(ids, member.Core)
			}
		}
	}
	slices.Sort(ids)
	columns := min(len(ids), 8)
	if class == wideLayout {
		var header strings.Builder
		header.WriteString(strings.Repeat(" ", 6))
		for _, id := range ids[:columns] {
			header.WriteString(grey.Render(fmt.Sprintf("%3d  ", id)))
		}
		header.WriteString(grey.Render("  found by              against the profile now"))
		out = append(out, header.String())
	}
	for _, combo := range s.combos {
		line := magenta.Render(fmt.Sprintf("C%d", combo.id))
		if class == wideLayout {
			line += strings.Repeat(" ", max(6-ansi.StringWidth(line), 1))
			for _, id := range ids[:columns] {
				value := ""
				for _, member := range combo.members {
					if member.Core == id {
						value = fmt.Sprint(member.Offset)
					}
				}
				style := grey
				if slices.Contains(combo.clear, id) {
					style = textStyle
					if combo.holds != nil {
						style = magenta
					}
				}
				line += style.Render(fmt.Sprintf("%3s  ", value))
			}
			how := "unresolved"
			if combo.probed {
				how = "probed"
			} else if combo.fallback {
				how = "fallback"
			}
			line += textStyle.Render(fmt.Sprintf("  hunt %d, %-12s", combo.hunt, how))
		} else {
			line += " " + grey.Render(compactMembers(combo.members)) + " "
		}
		if combo.holds != nil {
			line += magenta.Render(fmt.Sprintf("holds core %02d at %d", combo.holds.Core, combo.holds.Offset))
		} else {
			line += grey.Render("clear: ") + textStyle.Render(coreIDs(combo.clear)+" shallower")
		}
		out = append(out, trimWords(line, width))
	}
	if class == wideLayout {
		var line strings.Builder
		line.WriteString(white.Render("now") + "   ")
		for _, id := range ids[:columns] {
			value := ""
			if c := s.core(id); c != nil {
				value = fmt.Sprint(c.profile)
			}
			line.WriteString(white.Render(fmt.Sprintf("%3s  ", value)))
		}
		out = append(out, "", line.String())
	}
	return out
}

func (s Snapshot) huntLines(width int, class sizeClass, now time.Time) []string {
	h := s.hunt
	out := []string{rule(width, grey.Render(fmt.Sprintf("HUNT %d", h.id)), grey.Render("started "+h.started.Format("15:04")+" · "+age(now.Sub(h.started))+" so far")), ""}
	cause := signalWords[h.cause.signal] + " in " + trialDescription(h.cause.trial)
	if h.cause.carried {
		cause += " · carried trial"
	}
	if h.cause.rerunOf {
		cause += " · rerun"
	}
	if h.cause.core == nil {
		cause += " · no core named"
	}
	out = append(out, fieldLine("cause", cause, width))
	if class != compactLayout {
		for _, evidence := range h.evidence {
			out = append(out, fieldLine("evidence", vtText(evidence), width))
		}
		out = append(out, fieldLine("candidates", coreIDs(h.candidates)+", every core with an offset in the failing profile", width), "")
	}
	if len(h.plan) > 0 {
		if class == wideLayout {
			out = append(out, fieldLine("parts", "part   at failing offsets    parked   schedule   state", width))
		}
		for i, part := range h.plan {
			mark, state := " ", part.outcome
			if part.running {
				mark = "►"
				if s.trial != nil {
					state = fmt.Sprintf("trial %d/%d", s.trial.index, s.trial.of)
				}
			} else if part.group == 0 {
				state = "to come"
			}
			line := fmt.Sprintf("part %d/%d %s at failing offsets · %s parked · %d x %s · %s", i+1, len(h.plan), coreIDs(part.failing), coreIDs(part.parked), part.trials, shortDuration(part.length), state)
			out = append(out, trimWords(stageMarker(mark)+" "+textStyle.Render(line), width))
		}
	}
	if len(h.probes) > 0 {
		if class != compactLayout {
			out = append(out, "", fieldLine("probes", "member  now   group failed at      passed at           state", width))
		}
		for _, probe := range h.probes {
			state := "waiting"
			if probe.done {
				state = "done"
			} else if probe.running && s.trial != nil {
				state = fmt.Sprintf("► trial %d of %d", s.trial.index, s.trial.of)
			}
			line := fmt.Sprintf("%02d  %3d  failed %s · passed %s", probe.member, probe.now, offsetList(probe.failedAt), offsetList(probe.passedAt))
			if len(probe.carried) > 0 {
				line += " · carried " + offsetList(probe.carried)
			}
			out = append(out, fieldLine("", line+" · "+state, width))
		}
	}
	if class != compactLayout && len(h.groups) > 0 {
		out = append(out, "", fieldLine("groups", "most recent groups; carried answers need no trial", width))
		start := max(len(h.groups)-4, 0)
		for _, group := range h.groups[start:] {
			line := fmt.Sprintf("G%d %s · %s · %d/%d passed", group.id, coreIDs(group.cores), group.outcome, group.passes, group.needed)
			if group.inferred {
				line += " · carried"
			}
			out = append(out, fieldLine("", line, width))
		}
	}
	out = append(out, "", fieldLine("then", "after the hunt resolves:", width))
	rerun := fmt.Sprintf("rerun %s on %s: %d x %s", loadWords(h.rerun.regime), coreIDs(h.rerun.cores), h.rerun.short, shortDuration(h.rerun.shortLen))
	if h.rerun.long > 0 {
		rerun += fmt.Sprintf(", then %d x %s", h.rerun.long, shortDuration(h.rerun.longLen))
	}
	if h.rerun.short > 0 || h.rerun.long > 0 {
		out = append(out, fieldLine("", rerun, width))
	}
	if h.resume != nil {
		out = append(out, fieldLine("", "resume "+forecastTrial(*h.resume), width))
	}
	return out
}

func fieldLine(label, text string, width int) string {
	return trimWords(grey.Render(fmt.Sprintf("%-10s", label))+textStyle.Render(text), width)
}

func (s Snapshot) turnLines(width int, class sizeClass) []string {
	out := []string{rule(width, grey.Render("TURNS"), grey.Render("one core per turn, in this order")), ""}
	if class == wideLayout {
		out = append(out, tableRow(width, []int{2, 9, 40, 47, 73}, []string{"core", "this turn", "at", "workload", "so far"}, grey))
	}
	for _, turn := range s.turns {
		mark, style := " ", textStyle
		if turn.running {
			mark, style = "►", white
		}
		what := "search step: " + regimeList(turn.regimes)
		if turn.step == 1 {
			what = "search step: 1 count deeper"
		}
		if turn.confirm {
			what = "confirm: " + regimeList(turn.regimes)
		}
		sofar := "none yet"
		if core := s.core(turn.core); core != nil {
			sofar = ansi.Strip(core.noteLine(34))
		}
		if class == wideLayout {
			out = append(out, stageMarker(mark)+tableRow(width-1, []int{1, 8, 39, 46, 72}, []string{fmt.Sprintf("%02d", turn.core), what, fmt.Sprint(turn.offset), workloadLabel(turn.workload.ID), sofar}, style))
		} else {
			out = append(out, trimWords(stageMarker(mark)+" "+style.Render(fmt.Sprintf("%02d %d %s", turn.core, turn.offset, what))+" "+grey.Render(sofar), width))
		}
	}
	var found []int
	for _, core := range s.cores {
		if core.solo != nil {
			found = append(found, core.id)
		}
	}
	if len(found) > 0 {
		out = append(out, "", grey.Render("waiting for the cycles  ")+textStyle.Render(coreIDs(found))+grey.Render(" · solo limits found"))
	}
	return out
}

func (s Snapshot) deepenLines(width int) []string {
	d := s.deepen
	out := []string{rule(width, grey.Render(fmt.Sprintf("DEEPEN · ROUND %d", d.round)), ""), ""}
	if d.waiting {
		out = append(out, textStyle.Render("Waiting for a passed full cycle."))
	}
	out = append(out, fieldLine("has room", coreIDs(d.room)+" · in this order", width))
	for i, target := range d.target {
		if i >= len(s.cores) || target == s.cores[i].profile {
			continue
		}
		verb := "goes deeper"
		if target > s.cores[i].profile {
			verb = "yields"
		}
		out = append(out, fieldLine("move", fmt.Sprintf("core %02d %s: %d → %d", s.cores[i].id, verb, s.cores[i].profile, target), width))
	}
	out = append(out, "", grey.Render("CHECKS"))
	for _, check := range d.checks {
		out = append(out, trimWords(textStyle.Render(fmt.Sprintf("%s on %s  %d/%d passed", check.Regime, coreIDs(check.Cores), check.Passes, check.Needed))+"  "+grey.Render(workloadLabel(check.Workload)), width))
	}
	return out
}

func (s Snapshot) core(id int) *coreView {
	for i := range s.cores {
		if s.cores[i].id == id {
			return &s.cores[i]
		}
	}
	return nil
}

func offsetList(values []int) string {
	parts := make([]string, len(values))
	for i, value := range values {
		parts[i] = fmt.Sprint(value)
	}
	if len(parts) == 0 {
		return "-"
	}
	return strings.Join(parts, " ")
}

func numberList(values []int) string { return strings.ReplaceAll(offsetList(values), " ", ",") }

func compactMembers(members []journal.CombinationMember) string {
	parts := make([]string, len(members))
	for i, member := range members {
		parts[i] = fmt.Sprintf("%02d:%d", member.Core, member.Offset)
	}
	return strings.Join(parts, " ")
}

func coreIDs(ids []int) string {
	text := coreList(ids)
	text = strings.TrimPrefix(text, "cores ")
	text = strings.TrimPrefix(text, "core ")
	return strings.ReplaceAll(text, ", ", " ")
}

func regimeList(regimes []machine.Regime) string {
	parts := make([]string, len(regimes))
	for i, regime := range regimes {
		parts[i] = strings.TrimSuffix(loadWords(regime), " load")
	}
	return strings.Join(parts, ", then ")
}

func shortDuration(d time.Duration) string {
	if d > 0 && d%time.Minute == 0 {
		return fmt.Sprintf("%dm", int(d/time.Minute))
	}
	return fmt.Sprintf("%ds", max(int(d/time.Second), 0))
}

func consoleText(text string) string {
	const glyphs = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■ ☺☻♥♦♣♠•◘○◙♂♀♪♫☼►◄↕‼¶§▬↨↑↓→←∟↔▲▼"
	var b strings.Builder
	start := 0
	for i, ch := range text {
		if ch < 128 || strings.ContainsRune(glyphs, ch) {
			continue
		}
		b.WriteString(text[start:i])
		switch ch {
		case '×':
			b.WriteByte('x')
		case '…':
			b.WriteString("...")
		case '–', '—':
			b.WriteByte('-')
		case '‘', '’':
			b.WriteByte('\'')
		case '“', '”':
			b.WriteByte('"')
		default:
			fmt.Fprintf(&b, "\\u%04x", ch)
		}
		start = i + len(string(ch))
	}
	if start == 0 {
		return text
	}
	b.WriteString(text[start:])
	return b.String()
}

func trimWords(text string, width int) string {
	if width <= 0 {
		return ""
	}
	if ansi.StringWidth(text) <= width {
		return text
	}
	cut := ansi.Truncate(text, width, "")
	plainCut := ansi.Strip(cut)
	if i := strings.LastIndexByte(plainCut, ' '); i > 0 {
		return ansi.Truncate(cut, ansi.StringWidth(plainCut[:i]), "")
	}
	return cut
}

func wrapStyled(text string, width int, style lipgloss.Style) []string {
	if width <= 0 {
		return nil
	}
	var out []string
	for paragraph := range strings.SplitSeq(text, "\n") {
		line := ""
		for word := range strings.FieldsSeq(paragraph) {
			for ansi.StringWidth(word) > width {
				if line != "" {
					out = append(out, style.Render(line))
					line = ""
				}
				out = append(out, style.Render(ansi.Cut(word, 0, width)))
				word = ansi.Cut(word, width, ansi.StringWidth(word))
			}
			if word == "" {
				continue
			}
			switch {
			case line == "":
				line = word
			case ansi.StringWidth(line)+1+ansi.StringWidth(word) <= width:
				line += " " + word
			default:
				out = append(out, style.Render(line))
				line = word
			}
		}
		if line != "" || paragraph == "" {
			out = append(out, style.Render(line))
		}
	}
	return out
}

func scrollbar(page, total, top, scrolled int) []string {
	page = max(page, 0)
	bar := make([]string, page)
	if scrolled == 0 || total <= 0 {
		for i := range bar {
			bar[i] = " "
		}
		return bar
	}
	thumb := min(max(page*page/total, 1), page)
	start := top * (page - thumb) / max(scrolled, 1)
	for i := range bar {
		bar[i] = grey.Render("│")
		if i >= start && i < start+thumb {
			bar[i] = textStyle.Render("█")
		}
	}
	return bar
}

func fit(lines []string, width, height int) []string {
	rows := max(height-1, 0)
	out := make([]string, rows)
	for i := range min(len(lines), rows) {
		out[i] = ansi.Truncate(lines[i], max(width, 0), "")
	}
	return out
}

func ago(d time.Duration) string {
	d = max(d, 0)
	if d < time.Minute {
		return "a moment ago"
	}
	if d < time.Hour {
		return fmt.Sprintf("%dm ago", int(d/time.Minute))
	}
	return hm(d) + " ago"
}

func age(d time.Duration) string {
	d = max(d, 0)
	if d < time.Minute {
		return fmt.Sprintf("%ds", int(d/time.Second))
	}
	if d < time.Hour {
		return fmt.Sprintf("%dm%02ds", int(d/time.Minute), int(d/time.Second)%60)
	}
	return hm(d)
}

func clock(d time.Duration) string {
	d = max(d, 0)
	if d < time.Hour {
		return fmt.Sprintf("%d:%02d", int(d/time.Minute), int(d/time.Second)%60)
	}
	return fmt.Sprintf("%d:%02d:%02d", int(d/time.Hour), int(d/time.Minute)%60, int(d/time.Second)%60)
}

func hm(d time.Duration) string {
	d = max(d, 0)
	return fmt.Sprintf("%dh%02dm", int(d/time.Hour), int(d/time.Minute)%60)
}
