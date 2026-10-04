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
	brief                                          bool // the role sits beside the CCD name and notes are bare numbers
}

// column is a table column inside its panel: the cell it starts at and how many cells it may use. A column of width
// 0 is not shown at this size.
type column struct{ at, w int }

// tables holds every table's columns for the screen size, so a header and its rows always share them.
type tables struct {
	field  int      // where the value of a label/value row starts
	cycle  []column // marker, number, kind, where, workload, schedule, result
	parts  []column // hunt parts: marker, part, at failing offsets, parked, schedule, state
	groups []column // hunt groups: which, what, outcome
	probes []column // member, now, failed at, passed at, state
	steps  []column // the numbered steps after a hunt: number, text
	turns  []column // marker, core, this turn, at, workload, so far
	combos []column // name, member offsets, found by, against the profile now
	member int      // cells per member offset in the combinations table
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
	tables                        tables
}

func tableColumns(class sizeClass, width int) tables {
	rest := func(at int) column { return column{at, max(width-at, 0)} }
	none := column{}
	switch class {
	case wideLayout:
		return tables{
			field:  12,
			cycle:  []column{{0, 1}, {2, 2}, {6, 21}, {28, 23}, {52, 28}, {82, 13}, rest(97)},
			parts:  []column{{10, 1}, {12, 10}, {24, 19}, {45, 12}, {59, 9}, rest(70)},
			groups: []column{{12, 12}, {25, 44}, rest(70)},
			probes: []column{{12, 7}, {20, 5}, {26, 19}, {46, 22}, rest(69)},
			steps:  []column{{12, 2}, rest(15)},
			turns:  []column{{0, 1}, {2, 6}, {9, 30}, {40, 6}, {47, 25}, rest(73)},
			combos: []column{{0, 5}, {6, 40}, {48, 21}, rest(70)},
			member: 5,
		}
	case mediumLayout:
		return tables{
			field:  12,
			cycle:  []column{{0, 1}, {2, 2}, {5, 17}, {23, 19}, none, {43, 13}, rest(57)},
			parts:  []column{{10, 1}, {12, 7}, {20, 12}, {33, 12}, {46, 8}, rest(55)},
			groups: []column{{12, 12}, {25, 29}, rest(55)},
			probes: []column{{12, 5}, {18, 5}, {24, 12}, {37, 17}, rest(55)},
			steps:  []column{{12, 2}, rest(15)},
			turns:  []column{{0, 1}, {2, 3}, {6, 27}, {34, 4}, none, rest(39)},
			combos: []column{{0, 3}, {4, 32}, {37, 7}, rest(45)},
			member: 4,
		}
	case compactLayout:
	}
	return tables{
		field:  7,
		cycle:  []column{{0, 1}, {2, 2}, {5, 17}, {23, 13}, none, none, rest(37)},
		parts:  []column{{0, 1}, {2, 8}, {11, 25}, none, none, rest(37)},
		groups: []column{{0, 12}, {13, 23}, rest(37)},
		probes: []column{{2, 5}, {8, 4}, {13, 11}, {25, 11}, rest(37)},
		steps:  []column{{7, 2}, rest(10)},
		turns:  []column{{0, 1}, {2, 3}, {6, 26}, {33, 4}, none, rest(38)},
		combos: []column{{0, 3}, none, {4, 7}, rest(12)},
	}
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
	if s.resting() {
		_, _, lines := s.restingBand()
		nowH = len(lines)
		if p.class != compactLayout {
			nowH++
		}
		outcomeH = 0
		ccdY = nowY + nowH + 1
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
		col := coreColumns{2, 5, 9, 18, 25, 44, 50, true}
		if p.class == wideLayout {
			col = coreColumns{2, 6, 11, 21, 50, 73, 81, false}
		} else if p.class == mediumLayout || !side {
			col = coreColumns{2, 6, 11, 21, 25, 48, 56, false}
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
	p.tables = tableColumns(p.class, p.context.w)
	p.body = rectangle{margin, sayY, full, max(p.hint-sayY-1, 0)}
	return p
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
		if !s.session || s.problem != nil {
			c.rows(p.body, narratorLines(st, p.body.w, p.body.h, false))
		} else {
			c.rows(p.say, narratorLines(st, p.say.w, p.say.h, p.class == compactLayout))
		}
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
	return Drawn{Lines: fit(c.lines(), p.width, sc.Height), Scroll: scroll, Until: until}
}

func narrativeStyle(t tone) lipgloss.Style {
	if t == plainTone {
		return lit
	}
	return toneStyle(t)
}

// narratorLines draws the story as `▌  LABEL   text`, wrapped lines aligned under the text, never under the label.
// brief is the one line of a compact screen, with no label.
func narratorLines(st story, width, height int, brief bool) []string {
	bar := narrativeStyle(st.tone).Render("▌") + "  "
	if brief {
		text := st.brief
		if text == "" && len(st.lines) > 0 {
			text = st.lines[0]
		}
		return []string{bar + trimWords(textStyle.Render(text), max(width-3, 0))}
	}
	indent := ansi.StringWidth(st.label) + 3
	var wrapped []string
	for _, line := range st.lines {
		wrapped = append(wrapped, wrapStyled(line, max(width-3-indent, 1), textStyle)...)
	}
	if len(wrapped) > height {
		if brief := wrapStyled(st.brief, max(width-3-indent, 1), textStyle); st.brief != "" && len(brief) <= height {
			wrapped = brief
		} else {
			wrapped = wrapped[:0]
			for _, line := range st.lines {
				wrapped = append(wrapped, trimWords(textStyle.Render(line), max(width-3-indent, 1)))
			}
		}
	}
	out := make([]string, 0, min(len(wrapped), height))
	for i, line := range wrapped {
		if i == height {
			break
		}
		label := strings.Repeat(" ", indent)
		if i == 0 {
			label = narrativeStyle(st.tone).Render(st.label) + "   "
		}
		out = append(out, bar+label+line)
	}
	return out
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
		if where := s.huntStage(short); where != "" {
			hunt += amber.Render("  " + where)
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
			break
		}
		at := fmt.Sprintf("  paused at step %d of %d", min(g.current+1, len(g.steps)), len(g.steps))
		if g.current < len(g.steps) && len(g.steps[g.current].parts) > 1 {
			if part := firstOpenPart(g.steps[g.current]); part > 0 {
				at += fmt.Sprintf(", part %d", part)
			}
		}
		cycle += amber.Render(at)
	case s.phase == journal.PhaseChecking:
		cycle = lit.Render("► ") + white.Render(name)
		if short {
			break
		}
		step := min(g.current+1, len(g.steps))
		t := s.trial
		if t != nil && t.cycle == g.number && t.step > 0 {
			step = t.step
		}
		cycle += textStyle.Render(fmt.Sprintf("  step %d of %d", step, len(g.steps)))
		if t != nil && t.cycle == g.number && t.parts > 1 {
			cycle += textStyle.Render(fmt.Sprintf(" · part %d of %d", t.part, t.parts))
		}
		if t != nil && t.cycle == g.number && t.of > 1 {
			cycle += textStyle.Render(fmt.Sprintf(" · trial %d of %d", t.index, t.of))
		}
	default:
		cycle = green.Render("■ ") + textStyle.Render(name)
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

// resting is true when no trial is in flight to show, or the session has stopped, met a dead end or just recovered
// from a crash.
func (s Snapshot) resting() bool {
	return s.session && s.problem == nil && (s.deadEnd != nil || s.stopped != nil || s.recover != nil || s.trial == nil)
}

// restingBand is the NOW band when no trial runs: its title and the rows it needs.
func (s Snapshot) restingBand() (string, lipgloss.Style, []string) {
	switch {
	case s.deadEnd != nil:
		return "DEAD END", red, []string{strings.ReplaceAll(vtText(string(s.deadEnd.condition)), "_", " ") + ": " + vtText(s.deadEnd.detail)}
	case s.stopped != nil:
		lines := []string{s.stopped.at.Format("15:04:05") + " · " + stopWords(s.stopped.reason)}
		if s.stopped.saved {
			lines = append(lines, "Rows show the saved profile, not applied now.")
		}
		return "STOPPED", grey, lines
	case s.recover != nil:
		return "RECOVERED FROM A CRASH", amber, s.recoveredLines()
	}
	var lines []string
	if s.last != nil {
		lines = append(lines, "last: "+lastDescription(*s.last))
	}
	return "BETWEEN TRIALS", grey, append(lines, s.nextLine())
}

// recoveredLines say what the crash ended, how late, what the journal holds about it, and what runs next.
func (s Snapshot) recoveredLines() []string {
	r := s.recover
	detected := "detected " + r.bootAt.Format("15:04:05")
	if r.reset != "" && r.reset != machine.ResetUnknown {
		detected += " after a " + strings.ReplaceAll(string(r.reset), "_", " ") + " reset"
	}
	if r.trial == nil {
		return []string{"crash with no trial in flight · " + detected, s.nextLine()}
	}
	t := *r.trial
	first := "crash in " + trialName(t)
	if where := cyclePlace(t); where != "" {
		first += " · " + where
	}
	switch {
	case r.end != nil && r.end.core != nil:
		first += fmt.Sprintf(" · core %02d named", *r.end.core)
	case r.end != nil:
		first += " · no core named"
	}
	if t.recordOnly {
		first += " · record only"
	}
	var second []string
	if e := r.end; e != nil && e.lastSample != nil {
		second = append(second, lateness(e)+" the trial", "last sample "+clock(*e.lastSample)+" of "+clock(e.planned))
		if e.stalled != nil {
			second = append(second, fmt.Sprintf("core %02d's worker had stalled by then", *e.stalled))
		}
	}
	second = append(second, detected)
	return []string{first, strings.Join(second, " · "), s.nextLine()}
}

func drawRestingNow(c *canvas, p layout, s Snapshot) {
	r := p.now
	title, style, lines := s.restingBand()
	if p.class == compactLayout {
		c.put(r, 0, 0, style.Render(title)+"  "+textStyle.Render(lines[0]))
		for i, line := range lines[1:] {
			c.put(r, 0, i+1, textStyle.Render(line))
		}
		return
	}
	c.put(r, 0, 0, rule(r.w, style.Render(title), ""))
	for i, line := range lines {
		c.put(r, 0, i+1, textStyle.Render(line))
	}
}

func drawNow(c *canvas, p layout, s Snapshot, now time.Time) {
	r := p.now
	if s.deadEnd != nil || s.stopped != nil || s.recover != nil || s.trial == nil {
		drawRestingNow(c, p, s)
		return
	}
	t := s.trial
	op := s.operation(*t)
	description := chip.Render(" "+string(t.regime)+" ") + "  " + white.Render(loadWords(t.regime)) + "  " + textStyle.Render(s.trialWhere(*t)) + "   " + grey.Render(workloadDisplay(t.workload))
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
		parts := make([]string, len(rows))
		for i, o := range rows {
			parts[i] = o.style.Render(o.short) + " " + textStyle.Render(o.compact)
		}
		c.put(p.outcomes, 0, 0, strings.Join(parts, "   "))
		return
	}
	for i, o := range rows {
		c.put(p.outcomes, 0, i, o.style.Render(fmt.Sprintf("%-21s", o.label))+textStyle.Render(fitPhrases(o.phrases, max(p.outcomes.w-21, 0))))
	}
}

func drawCCD(c *canvas, r rectangle, col coreColumns, id int, s Snapshot) {
	role, short := s.ccdRole(id)
	if col.brief {
		name := fmt.Sprintf("CCD %d", id)
		c.put(r, 0, 0, white.Render(name)+" "+grey.Render(trimWords(short, col.gauge-len(name)-3)))
	} else {
		c.put(r, 0, 0, white.Render(fmt.Sprintf("CCD %d", id)))
	}
	c.put(r, col.gauge-1, 0, grey.Render("0"))
	if col.cells == 50 {
		for n := 10; n <= 50; n += 10 {
			c.put(r, col.gauge+n-3, 0, grey.Render(fmt.Sprintf("-%d", n)))
		}
	} else {
		c.put(r, col.gauge+2, 0, track.Render("2 counts/cell"))
		c.put(r, col.gauge+col.cells-3, 0, grey.Render("-50"))
	}
	if col.brief {
		c.put(r, col.failure, 0, grey.Render("fail"))
		if slices.ContainsFunc(s.cores, func(core coreView) bool { return core.ccd == id && core.returnsTo != nil }) {
			c.put(r, col.note, 0, grey.Render("back"))
		}
	} else {
		c.put(r, col.failure-1, 0, grey.Render("fail pt"))
		if ansi.StringWidth(role) > r.w-col.note {
			role = short
		}
		c.put(r, col.note, 0, grey.Render(role))
	}
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
			failStyle := grey
			if core.judged {
				failStyle = textStyle
			}
			c.put(r, col.failure, y, failStyle.Render(fmt.Sprintf("%5d", *core.fail)))
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

// ccdRole says what this trial does with a CCD's cores, in full and in a word.
func (s Snapshot) ccdRole(id int) (string, string) {
	loaded, judged, parked, total, suspects, members := 0, 0, 0, 0, 0, 0
	atZero, parkedZero := true, true
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
		switch c.state {
		case coreParked:
			parked++
			parkedZero = parkedZero && offset == 0
		case coreSuspect:
			suspects++
		case coreMember, coreProbe:
			members++
		case coreWaiting, coreSearch, coreConfirm, coreFound, coreAtLimit, coreHasRoom:
		}
	}
	parkedWords := "parked"
	if parkedZero {
		parkedWords = "parked at 0"
	}
	switch {
	case total == 0:
		return "", ""
	case parked == total && loaded == total:
		return parkedWords + ", still under load", "parked"
	case parked == total:
		return parkedWords + ", idle", "parked"
	case loaded == total && suspects == total:
		return "suspects at their failing offsets", "suspects"
	case loaded == total && members == total:
		return "members at their failing offsets", "members"
	case loaded == total:
		return "all under load", "all loaded"
	case judged == total && loaded == 0:
		return "all idle, all judged", "judged"
	case loaded == 0 && atZero:
		return "waiting at 0", "waiting"
	case loaded == 0:
		return "not under load", "idle"
	case loaded == 1:
		return "one core under load at a time", "one loaded"
	}
	return fmt.Sprintf("%d under load · %d judged", loaded, judged), fmt.Sprintf("%d loaded", loaded)
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
		before, alarm, after := e.sentenceParts()
		text := textStyle.Render(before) + red.Render(alarm) + textStyle.Render(after)
		out = append(out, grey.Render(e.at.Format("15:04"))+"  "+tagStyle(e.tag).Render(fmt.Sprintf("%-8s", e.tag))+" "+trimWords(text, max(width-16, 0)))
	}
	return out
}

// tagStyle colours a history tag by what kind of event it names, so the eye finds a crash or a hunt down the column.
func tagStyle(tag string) lipgloss.Style {
	switch tag {
	case tagPass:
		return green
	case tagCrash, tagFail:
		return red
	case tagHunt, tagGroup, tagBackoff, tagProbe, tagMCE, tagNote:
		return amber
	case tagCombo:
		return magenta
	case tagStart, tagSkip, tagRecord, tagResume, tagReset:
		return grey
	}
	return textStyle
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

func coreIDs(ids []int) string {
	text := coreList(ids)
	text = strings.TrimPrefix(text, "cores ")
	text = strings.TrimPrefix(text, "core ")
	return strings.ReplaceAll(text, ", ", " ")
}

func regimeList(regimes []machine.Regime) string {
	parts := make([]string, len(regimes))
	for i, regime := range regimes {
		parts[i] = strings.TrimSuffix(kindWords(regime), " vector")
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
		kept := strings.TrimRight(plainCut[:i], " ·→,;:")
		return ansi.Truncate(cut, ansi.StringWidth(kept), "")
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
