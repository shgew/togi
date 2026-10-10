package watch

import (
	"cmp"
	"fmt"
	"slices"
	"strings"
	"time"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/render"
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

// Scroll counts lines from the top; a negative value selects the end. OneFrame is a frame printed once, which says
// nothing of the journal changing later.
type Screen struct {
	View          View
	Keys          bool
	Scroll        int
	Width, Height int
	OneFrame      bool
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
		field:  11,
		cycle:  []column{{0, 1}, {2, 2}, {5, 17}, {23, 13}, none, none, rest(37)},
		parts:  []column{{0, 1}, {2, 8}, {11, 25}, none, none, rest(37)},
		groups: []column{{0, 12}, {13, 23}, rest(37)},
		probes: []column{{2, 5}, {8, 4}, {13, 11}, {25, 11}, rest(37)},
		steps:  []column{{11, 2}, rest(14)},
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
	title = cutWords(title, width)
	tw, rw := ansi.StringWidth(title), ansi.StringWidth(right)
	if rw == 0 || tw+rw+2 > width {
		if tw == width {
			return title
		}
		return title + " " + track.Render(strings.Repeat("─", max(width-tw-1, 0)))
	}
	return title + " " + track.Render(strings.Repeat("─", width-tw-rw-2)) + " " + right
}

func Render(s Snapshot, w, h int, now time.Time) string {
	return strings.Join(RenderView(s, Screen{View: MainView, Width: w, Height: h, OneFrame: true}, now).Lines, "\n")
}

// runMark is the stage marker of a stage the tuner is in: a running ► in style, or a plain ○ once the session stopped
// or its trial crashed.
func (s Snapshot) runMark(running lipgloss.Style) string {
	if s.stopped != nil || s.crashedTrial() {
		return track.Render("○ ")
	}
	return running.Render("► ")
}

func RenderView(s Snapshot, sc Screen, now time.Time) Drawn {
	p := measure(s, sc)
	c := newCanvas(p)
	var until time.Time
	planned := s.quietUntil()
	if !planned.IsZero() {
		now, until = s.trial.started, heldUntilRecorded
	}
	c.put(p.header, 0, 0, s.header(now, p.header.w, p.class == compactLayout))
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
		st := s.story(now, sc.OneFrame)
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
			c.rows(p.history, s.historyPanel(p.history.w, p.history.h))
		}
	}
	if sc.Keys {
		var arrival []clause
		if sc.View == LogView && s.logArrived > 0 {
			count := plural(s.logArrived, "new event")
			arrival = []clause{{"   ", []form{{amber.Render(count + " · End: latest"), 2}, {amber.Render(count), 2}, {}}}}
		}
		hints := keyHints(sc.View, p.header.w, arrival...)
		c.put(rectangle{p.header.x, p.hint, p.header.w, 1}, 0, 0, hints)
	}
	return Drawn{Lines: fit(c.lines(), p.width, sc.Height), Scroll: scroll, Until: until}
}

// r7Lines lists, under their own rule once checking has a cycle, every R7 workload's current top requesters and its
// self-sufficient cores. A CCD without request measurements names its top requesters by offset.
func (s Snapshot) r7Lines(p layout) []string {
	if s.cycle == nil || len(s.r7) == 0 {
		return nil
	}
	width := p.context.w
	name, top := 17, 17
	switch {
	case p.class == wideLayout && width >= 100:
		name, top = 30, 24
	case p.class == mediumLayout && width >= 70:
		name, top = 24, 20
	}
	if width < name+top+1 {
		name = min(name, max(width-1-len("by offset"), 0))
		top = width - name
		if name > 0 {
			top--
		}
		top = max(top, 0)
	}
	current := s.r7Workload()
	out := []string{r7Rule(width, name, top)}
	for _, w := range machine.Workloads(machine.R7) {
		var measured, byOffset, self []int
		total := 0
		for _, c := range s.r7 {
			if c.Workload != w.ID {
				continue
			}
			total++
			if c.TopRequester && c.OffsetFallback {
				byOffset = append(byOffset, c.Core)
			} else if c.TopRequester {
				measured = append(measured, c.Core)
			}
			if c.SelfSufficient {
				self = append(self, c.Core)
			}
		}
		if total == 0 {
			continue
		}
		label := workloadDisplayID(w.ID)
		if ansi.StringWidth(label) > name-1 {
			words := strings.Fields(label)
			label = strings.Join(words[:min(len(words), 2)], " ")
		}
		suff := "none yet"
		switch {
		case len(self) == total:
			suff = "all"
		case len(self) > 0:
			suff = coreIDs(self)
		}
		if ansi.StringWidth(suff) > width-name-top-2 {
			suff = fmt.Sprintf("%d of %d cores", len(self), total)
		}
		style := grey
		if w.ID == current {
			style = textStyle
		}
		line := fitClauses(width,
			whole("", asciiCell(label, name)),
			whole(" ", asciiCell(topCell(measured, byOffset, top), top)),
			clause{" ", []form{{suff, 1}, {}}},
		)
		out = append(out, style.Render(line))
	}
	return out
}

// topCell fits a workload's top requesters to width. Offset proxies keep their label when either list is cut, and
// the measured list is cut first, so it never takes the offset label.
func topCell(measured, byOffset []int, width int) string {
	const by = " by offset"
	m, o := coreIDs(measured), coreIDs(byOffset)
	if len(byOffset) == 0 {
		return cutWords(cmp.Or(m, "-"), width)
	}
	if len(measured) > 0 {
		if both := m + "; " + o + by; ansi.StringWidth(both) <= width {
			return both
		}
		if kept := cutWords(m, width-len("; ")-ansi.StringWidth(o+by)); kept != "" {
			return kept + "; " + o + by
		}
	}
	if kept := cutWords(o, width-len(by)); kept != "" {
		return kept + by
	}
	if width >= len("by offset") {
		return "by offset"
	}
	return ""
}

// r7Rule heads the R7 lines with its column names; on a wide panel it says the evidence is no guarantee.
func r7Rule(width, name, top int) string {
	topLabel := "top"
	if top >= 20 {
		topLabel = "top requesters"
	}
	dashes := func(n int) string { return track.Render(strings.Repeat("─", max(n, 1))) }
	line := grey.Render("R7") + " " + dashes(name-3) + " " + grey.Render(topLabel) + " " + dashes(top-len(topLabel)-1) + " " + grey.Render("self-sufficient")
	const note = "observed, not a guarantee"
	if rest := width - ansi.StringWidth(line) - 1; rest >= len(note)+3 {
		return line + " " + dashes(rest-len(note)-1) + " " + grey.Render(note)
	}
	if rest := width - ansi.StringWidth(line) - 1; rest > 0 {
		return line + " " + dashes(rest)
	}
	if ansi.StringWidth(line) <= width {
		return line
	}
	line = fitClauses(width,
		whole("", grey.Render("R7")),
		clause{" ", []form{{grey.Render(topLabel), 2}, {grey.Render("top"), 1}, {}}},
		clause{" ", []form{{grey.Render("self-sufficient"), 1}, {}}},
	)
	if rest := width - ansi.StringWidth(line) - 1; rest > 0 {
		line += " " + dashes(rest)
	}
	return line
}

// asciiCell pads whole-word fitted text to exactly width cells.
func asciiCell(text string, width int) string {
	text = cutWords(text, width)
	return text + strings.Repeat(" ", max(width-ansi.StringWidth(text), 0))
}

// r7Workload is the R7 workload the running trial, or else the cycle's current step, loads; empty outside R7.
func (s Snapshot) r7Workload() string {
	if t := s.trial; t != nil && t.regime == machine.R7 {
		return t.workload.ID
	}
	if g := s.cycle; g != nil && g.current < len(g.steps) && g.steps[g.current].regime == machine.R7 {
		return g.steps[g.current].workload.ID
	}
	return ""
}

// r7Top maps the current R7 workload's top requesters to whether offsets stand in for their requests.
func (s Snapshot) r7Top() map[int]bool {
	w := s.r7Workload()
	if w == "" {
		return nil
	}
	top := map[int]bool{}
	for _, c := range s.r7 {
		if c.Workload == w && c.TopRequester {
			top[c.Core] = c.OffsetFallback
		}
	}
	return top
}

// requestBasis says where a running multi-core R7 trial's forecast decisions take top requesters from: its end is
// forecast without telemetry, so they come from earlier requests, or offset order where none were measured.
func requestBasis(branch outcome) string {
	if branch.offsetOrder {
		return "per offset order"
	}
	return "per earlier requests"
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
		return []string{bar + cutWords(textStyle.Render(text), max(width-3, 0))}
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
				wrapped = append(wrapped, cutWords(textStyle.Render(line), max(width-3-indent, 1)))
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

// heldUntilRecorded is the Until of a started idle trial's frames: they change only with the journal, keys or a resize,
// past the trial's planned end until its end is recorded, so drawing never wakes the idle cores.
var heldUntilRecorded = time.Date(9999, time.January, 1, 0, 0, 0, 0, time.UTC)

// quietUntil is a started idle trial's planned end, or zero when no idle trial is running.
func (s Snapshot) quietUntil() time.Time {
	if s.problem == nil && s.session && s.deadEnd == nil && s.stopped == nil && s.trial != nil && !s.trial.crashed && s.trial.hasStarted && s.trial.regime == machine.R6 && s.trial.duration > 0 {
		return s.trial.started.Add(s.trial.duration)
	}
	return time.Time{}
}

// header is the top line fitted to width cells. Drop order, first to last: how long ago the last failure was and
// then its time, "session" before the session's length, the words after the failure and crash counts, the pause
// notice. The clock, the session's length and the counts are never left out. A compact screen paused for an idle
// trial names the pause beside the clock.
func (s Snapshot) header(now time.Time, width int, compact bool) string {
	const gap = "   "
	head := grey.Render("togi") + gap + white.Render(wallSecond(now))
	if s.starting {
		return fitClauses(width, whole("", head), whole("", grey.Render("   starting")))
	}
	if s.problem != nil {
		return fitClauses(width, whole("", head), whole("", grey.Render("   can't read journal")))
	}
	if !s.session {
		return fitClauses(width, whole("", head), whole("", grey.Render("   no session yet")))
	}
	end := now
	if s.deadEnd != nil {
		end = s.deadEnd.at
	} else if s.stopped != nil {
		end = s.stopped.at
	}
	paused := time.Time{}
	if until := s.quietUntil(); !until.IsZero() && now.Before(until) {
		paused = until
	}
	clauses := []clause{whole("", head)}
	pausedBeside := compact && !s.quietUntil().IsZero()
	if pausedBeside {
		// A compact screen held for an idle trial states the planned end, not the screen's wall clock.
		clauses = append(clauses, clause{gap, []form{{amber.Render("paused until " + wallMinute(s.quietUntil())), 0}}})
		paused = time.Time{}
	}
	hours := hm(end.Sub(s.start))
	clauses = append(clauses,
		clause{gap, []form{{grey.Render("session ") + textStyle.Render(hours), 2}, {textStyle.Render(hours), 0}}},
		clause{gap, []form{
			{textStyle.Render(fmt.Sprint(s.failures)) + grey.Render(" "+noun(s.failures, "failure")+" · ") + textStyle.Render(fmt.Sprint(s.crashes)) + grey.Render(" "+noun(s.crashes, "crash")), 1},
			{textStyle.Render(fmt.Sprint(s.failures)) + grey.Render(" fail · ") + textStyle.Render(fmt.Sprint(s.crashes)) + grey.Render(" crash"), 0},
		}})
	if s.lastFailure != nil && !pausedBeside {
		at := textStyle.Render(wallMinute(*s.lastFailure))
		forms := []form{{grey.Render("last observed failure ") + at, 5}, {}}
		if s.quietUntil().IsZero() {
			forms = append([]form{{grey.Render("last observed failure ") + at + grey.Render(", "+ago(now.Sub(*s.lastFailure))), 6}}, forms...)
		}
		clauses = append(clauses, clause{gap, forms})
	}
	if !paused.IsZero() {
		clauses = append(clauses, clause{gap, []form{{amber.Render("screen paused until " + wallMinute(paused)), 4}, {amber.Render("paused until " + wallMinute(paused)), 3}, {}}})
	}
	return fitClauses(width, clauses...)
}

func (s Snapshot) stageLine(class sizeClass, summary bool, now time.Time) string {
	if !s.session || s.problem != nil {
		return ""
	}
	found := 0
	for _, c := range s.cores {
		if c.solo != nil {
			found++
		}
	}
	short := class == compactLayout
	names := []string{"SOLO LIMITS", "PHASE 1", "PHASE 2", "CLEAN CYCLES", "CYCLE"}
	join := "  " + track.Render("───") + "  "
	if short {
		names = []string{"SOLO", "P1", "P2", "CLEAN", "CYCLE"}
		join = " " + track.Render("─") + " "
	}
	parts := []string{s.soloStage(names[0], found, short)}
	if h := s.hunt; h != nil {
		hunt := s.runMark(amber) + lamp.Render(fmt.Sprintf(" HUNT %d ", h.id))
		if where := s.huntStage(short); where != "" {
			hunt += amber.Render("  " + where)
		}
		parts = append(parts, hunt)
	}
	parts = append(parts, s.phase1Stage(names[1], found), s.phase2Stage(names[2], short), grey.Render(fmt.Sprintf("%s %d", names[3], s.cleanCycles)))
	if !short && s.stopped == nil && s.phases != nil && s.phases.phase == 0 {
		parts[len(parts)-1] += track.Render("  · repeats until stopped")
	}
	parts = append(parts, s.cycleStage(names[4], short))
	if summary && s.trial != nil && s.trial.hasStarted {
		t := s.trial
		if until := s.quietUntil(); !until.IsZero() && now.Before(until) {
			parts = append(parts, grey.Render("screen paused until "+wallMinute(until)))
		} else if t.crashed {
			parts = append(parts, amber.Render("trial crashed"))
		} else if past := t.pastEnd(now); past > 0 {
			parts = append(parts, amber.Render(clock(past)+" past its end"))
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
	solo := s.runMark(lit) + white.Render(name) + textStyle.Render(fmt.Sprintf("  %d of %d found", found, len(s.cores)))
	if s.trial != nil && s.trial.condition == machine.Alone && !short && !s.trial.crashed {
		verb := "searching"
		if c := s.core(s.trial.core); c != nil && c.confirm != nil {
			verb = "confirming"
		}
		solo += textStyle.Render(fmt.Sprintf(" · core %02d %s %d", s.trial.core, verb, s.trial.offset))
	}
	return solo
}

// phase1Stage is search plus the first cycle: it ends at that cycle's pass, which confirms the first BIOS profile.
func (s Snapshot) phase1Stage(name string, found int) string {
	switch {
	case s.phases == nil || s.phases.phase == 1 && (found < len(s.cores) || s.hunt != nil):
		return grey.Render("○ " + name)
	case s.phases.phase == 1:
		return s.runMark(lit) + white.Render(name)
	}
	return green.Render("■ ") + textStyle.Render(name)
}

// phase2Stage is the rounds and the confirmation cycle, and in a round or between rounds the most that is left.
func (s Snapshot) phase2Stage(name string, short bool) string {
	ph := s.phases
	switch {
	case ph == nil || ph.phase == 1:
		return grey.Render("○ " + name)
	case ph.phase == 0:
		stage := green.Render("■ ") + textStyle.Render(name)
		if !short {
			stage += grey.Render("  concluded")
		}
		return stage
	}
	stage := s.runMark(lit) + white.Render(name)
	if s.hunt != nil {
		stage = amber.Render("○ ") + textStyle.Render(name)
	}
	switch {
	case ph.round > 0 && short:
		stage += textStyle.Render(fmt.Sprintf(" r%d", ph.round))
	case ph.round > 0:
		stage += textStyle.Render(fmt.Sprintf("  round %d · at most %d left", ph.round, ph.roundsLeft))
	case ph.confirming && !short:
		stage += textStyle.Render("  confirmation cycle")
	case !ph.confirming && !short:
		stage += textStyle.Render(fmt.Sprintf("  at most %s left", plural(ph.roundsLeft, "round")))
	}
	return stage
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
			cycle += amber.Render(fmt.Sprintf(" paused at %d/%d", min(g.resumeStep()+1, len(g.steps)), len(g.steps)))
			break
		}
		resume := g.resumeStep()
		at := fmt.Sprintf("  paused at step %d of %d", min(resume+1, len(g.steps)), len(g.steps))
		if resume < len(g.steps) && len(g.steps[resume].parts) > 1 {
			if part := firstOpenPart(g.steps[resume]); part > 0 {
				at += fmt.Sprintf(", part %d", part)
			}
		}
		cycle += amber.Render(at)
	case s.stopped != nil && g.current >= len(g.steps):
		cycle = green.Render("■ ") + textStyle.Render(name)
	case s.phase == journal.PhaseChecking:
		cycle = s.runMark(lit) + white.Render(name)
		if short {
			break
		}
		step := min(g.resumeStep()+1, len(g.steps))
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

func keyHints(view View, width int, after ...clause) string {
	type key struct {
		key, label      string
		active, current bool
	}
	keys := []key{{"?", "help", true, view == HelpView}, {"l", "log", true, view == LogView}, {"esc", "back", view != MainView, false}, {"q", "close view", true, false}}
	clauses := make([]clause, 0, len(keys)+len(after))
	for _, k := range keys {
		style, label := chip, grey
		if k.current {
			style = plain.Foreground(lipgloss.Color("0")).Background(lipgloss.Color("7"))
		} else if !k.active {
			style, label = track, track
		}
		forms := []form{{style.Render(" "+k.key+" ") + " " + label.Render(k.label), 10}}
		if k.key == "q" {
			forms = append(forms, form{style.Render(" q ") + " " + label.Render("close"), 10})
		}
		forms = append(forms, form{style.Render(k.key), 1})
		if k.key != "q" {
			forms = append(forms, form{})
		}
		clauses = append(clauses, clause{"   ", forms})
	}
	return fitClauses(width, append(clauses, after...)...)
}

// resting is true when no trial is in flight to show, or the session has stopped, met a dead end or just recovered
// from a crash.
func (s Snapshot) resting() bool {
	return s.session && s.problem == nil && (s.deadEnd != nil || s.stopped != nil || s.recover != nil || s.crashedTrial() || s.trial == nil)
}

// restingBand is the NOW band when no trial runs: its title and the rows it needs.
func (s Snapshot) restingBand() (string, lipgloss.Style, []string) {
	switch {
	case s.deadEnd != nil:
		lines := []string{strings.ReplaceAll(vtText(string(s.deadEnd.condition)), "_", " ") + ": " + vtText(s.deadEnd.detail)}
		if s.stopped != nil && s.stopped.saved {
			lines = append(lines, "Rows show the saved profile, not applied now.")
		}
		return "DEAD END", red, lines
	case s.crashedTrial():
		return "CRASHED, NOT YET RECOVERED", amber, s.crashedLines()
	case s.stopped != nil:
		lines := []string{wallSecond(s.stopped.at) + " · " + stopWords(s.stopped.reason)}
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
	detected := "detected " + wallSecond(r.bootAt)
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
	case r.end != nil && t.hunt == 0 && t.condition != machine.Alone:
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

// crashedTrial is true when the journal's tail is a trial whose boot a later boot has outlived: the trial crashed, and
// the next togi run has yet to record it.
func (s Snapshot) crashedTrial() bool { return s.trial != nil && s.trial.crashed }

// crashedLines say which trial the journal's later boot ended and what records the crash.
func (s Snapshot) crashedLines() []string {
	t := *s.trial
	first := "trial " + vtText(t.id) + " · " + trialName(t)
	if where := cyclePlace(t); where != "" {
		first += " · " + where
	}
	if t.hasStarted {
		first += " · started " + wallSecond(t.started)
	}
	return []string{first, "A later boot has written events, so the machine restarted before this trial recorded its end.", "togi run records the crash and picks up from the journal."}
}

// pastEnd is how long a started trial is past its planned end without a recorded end, or zero. A crashed trial has no
// end to be past.
func (t trialView) pastEnd(now time.Time) time.Duration {
	if !t.hasStarted || t.crashed || t.duration <= 0 {
		return 0
	}
	if past := now.Sub(t.started.Add(t.duration)); past >= time.Second {
		return past
	}
	return 0
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
	if s.deadEnd != nil || s.stopped != nil || s.recover != nil || s.crashedTrial() || s.trial == nil {
		drawRestingNow(c, p, s)
		return
	}
	t := s.trial
	op := s.operation(*t)
	// Left out first to last: the worker count, the workload, what the load is called; the chip and where the trial
	// loads stay.
	leave := func(sep, text string, rank int) clause { return clause{sep, []form{{text, rank}, {}}} }
	description := []clause{
		whole("", chip.Render(" "+string(t.regime)+" ")),
		leave("  ", white.Render(loadWords(t.regime)), 2),
		whole("  ", textStyle.Render(s.trialWhere(*t))),
		leave("   ", grey.Render(workloadDisplay(t.workload)), 3),
	}
	if workers := len(t.cores) * t.workload.Threads; workers > 0 {
		suffix := "s"
		if workers == 1 {
			suffix = ""
		}
		description = append(description, leave("", grey.Render(fmt.Sprintf(" · %d worker%s", workers, suffix)), 4))
	}
	quiet := t.hasStarted && t.regime == machine.R6 && now.Before(t.started.Add(t.duration))
	if quiet {
		description = []clause{
			whole("", chip.Render(" R6 ")),
			leave("  ", white.Render("idle + bursts"), 2),
			whole("  ", textStyle.Render("on "+coresText(t.cores, len(s.cores)))),
			leave("   ", grey.Render("short wake-ups, no workers in between"), 3),
		}
	}
	if p.class == wideLayout || p.class == mediumLayout {
		c.put(r, 0, 0, rule(r.w, grey.Render("NOW"), grey.Render("trial "+vtText(t.id))))
	}
	descY := 0
	if p.class != compactLayout {
		descY = 1
	}
	opWidth := ansi.StringWidth(op)
	c.put(r, 0, descY, fitClauses(max(r.w-opWidth-3, 0), description...))
	c.put(r, max(r.w-opWidth, 0), descY, op)
	if !t.hasStarted {
		c.put(r, 0, r.h-1, grey.Render("preparing · workload has not started yet · planned "+clock(t.duration)))
		return
	}
	elapsed := min(max(now.Sub(t.started), 0), max(t.duration, 0))
	remaining := max(t.duration-elapsed, 0)
	past := t.pastEnd(now)
	frac := 0.0
	if t.duration > 0 && !quiet {
		frac = float64(elapsed) / float64(t.duration)
	}
	if p.class == wideLayout {
		cells := max(r.w-23, 0)
		digits, style := clock(remaining), white
		label := "left"
		if past > 0 {
			digits, style, label = clock(past), amber, "past end"
		}
		if quiet {
			digits, style = wallMinute(t.started.Add(t.duration)), grey
		}
		for y := 3; y <= 5; y++ {
			done, rest := "█", "░"
			if y == 5 {
				done, rest = "▀", "▀"
			}
			c.putCells(r, 0, y, progressBar(cells, frac, done, rest))
		}
		for y, line := range bigDigits(digits) {
			c.putCells(r, r.w-ansi.StringWidth(line), 3+y, style.Render(line))
		}
		if quiet {
			msg := "  screen paused until " + digits + " so it cannot wake the idle cores  "
			c.put(r, max((cells-ansi.StringWidth(msg))/2, 0), 4, textStyle.Render(msg))
			c.put(r, 0, 6, grey.Render("started ")+textStyle.Render(wallMinute(t.started))+grey.Render(" · "+clock(t.duration)))
			c.put(r, r.w-7, 6, grey.Render("ends at"))
		} else {
			c.put(r, 0, 6, textStyle.Render(clock(elapsed))+grey.Render(" of "+clock(t.duration)))
			trials := requirementLine(*t, false)
			c.put(r, max(cells-ansi.StringWidth(trials), 0), 6, trials)
			c.put(r, r.w-ansi.StringWidth(label), 6, grey.Render(label))
		}
	} else {
		y := r.h - 1
		label := white.Render(clock(remaining)) + grey.Render(" left") + "  " + requirementLine(*t, true)
		if past > 0 {
			label = amber.Render(clock(past)+" past its end") + "  " + requirementLine(*t, true)
		}
		if quiet {
			label = grey.Render("screen paused · ends at ") + textStyle.Render(wallMinute(t.started.Add(t.duration)))
		}
		cells := max(r.w-ansi.StringWidth(label)-2, 0)
		c.putCells(r, 0, y, progressBar(cells, frac, "█", "░")+"  "+label)
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

// progressBar draws the elapsed fraction as done cells and the rest as rest cells, so the shape
// shows elapsed time without colour; a quiet R6 trial passes 0 and shows only the rest.
func progressBar(cells int, frac float64, done, rest string) string {
	cells = max(cells, 0)
	n := int(float64(cells) * min(max(frac, 0), 1))
	return lit.Render(strings.Repeat(done, n)) + track.Render(strings.Repeat(rest, cells-n))
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
	if s.trial == nil || s.stopped != nil || s.deadEnd != nil || s.recover != nil || s.crashedTrial() {
		return
	}
	rows := s.outcomeRows()
	if p.class == compactLayout {
		parts := make([]string, len(rows))
		basis := ""
		join := func(level int) string {
			for i, o := range rows {
				text := o.compact
				if level >= 1 {
					text = o.brief
				}
				if level >= 2 {
					text = tightNext(text)
				}
				parts[i] = o.style.Render(o.short) + " " + textStyle.Render(text)
				basis = cmp.Or(basis, o.basis)
			}
			return strings.Join(parts, "   ")
		}
		// Each level keeps less of the outcomes and never the next trial's core: first the folded cores at 0 are
		// counted rather than spelled out, then the next trial is named by its regime and core alone.
		line := join(0)
		for level := 1; level <= 2 && ansi.StringWidth(line) > p.outcomes.w; level++ {
			line = join(level)
		}
		if basis != "" {
			// The basis must stay on screen: it marks backoffs that new telemetry can change.
			switch room := p.outcomes.w - ansi.StringWidth(line) - 3; {
			case room >= len("backoffs ")+len(basis):
				line += "   " + grey.Render("backoffs "+basis)
			case room >= len(basis):
				line += "   " + grey.Render(basis)
			default:
				for i, o := range rows {
					if o.basis != "" {
						parts[i] += textStyle.Render("?")
					}
				}
				line = strings.Join(parts, "   ")
			}
		}
		c.put(p.outcomes, 0, 0, line)
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
		c.put(r, 0, 0, white.Render(name)+" "+grey.Render(cutWords(short, col.gauge-len(name)-3)))
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
		c.put(r, col.note, 0, grey.Render(fitClauses(r.w-col.note, clause{"", []form{{role, 1}, {short, 0}}})))
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
	top := s.r7Top()
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
		width := max(r.w-col.note, 0)
		note := core.noteLine(width)
		if byOffset, ok := top[core.id]; ok {
			note = topNote(note, byOffset, width)
		}
		c.put(r, col.note, y, cutWords(note, width))
	}
}

// topNote marks a top requester of the current R7 workload after the core's own note, in "top" alone on a column too
// narrow for the words. The mark is left out when it does not fit beside the note, and "top" that does not fit
// alone is not drawn.
func topNote(note string, byOffset bool, width int) string {
	word := "top"
	if width >= len("top requester") {
		word = "top requester"
		if byOffset {
			word = "top by offset"
		}
	}
	if note == "" {
		return textStyle.Render(fitClauses(width, whole("", word)))
	}
	return fitClauses(width, whole("", note), clause{grey.Render(" · "), []form{{textStyle.Render(word), 1}, {}}})
}

func (c coreView) stateWord() string {
	switch c.state {
	case coreWaiting:
		return "WAITING"
	case coreSearch:
		return render.PhaseWord(journal.PhaseSearch)
	case coreConfirm:
		return "CONFIRM"
	case coreFound:
		return "FOUND"
	case coreAtLimit:
		return render.PhaseWord(journal.PhaseAtLimit)
	case coreHasRoom:
		return render.PhaseWord(journal.PhaseHasRoom)
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
	// A combination holds the core at its profile offset: the depth it blocks
	// runs from one count past that offset to the solo limit.
	heldFrom, heldTo := 1, 0
	if c.holder != nil && c.holder.combination > 0 {
		heldFrom, heldTo = -c.profile+1, -back
		if c.solo != nil {
			heldTo = -*c.solo
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
		case max(lo, heldFrom) <= min(hi, heldTo):
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

// noteLine is the note beside a core's gauge, fitted to width cells. Each line gives its clauses in drop order: a
// value (an offset, a core, a count) is never left out, only its label shortens; context goes first.
func (c coreView) noteLine(width int) string {
	sep := " · "
	short := func(style lipgloss.Style, clauses ...clause) string {
		return style.Render(fitClauses(width, clauses...))
	}
	one := func(forms ...form) clause { return clause{sep, forms} }
	switch {
	case c.state == coreProbe && len(c.groupFails) > 0:
		list := offsetList(c.groupFails)
		return short(textStyle, one(form{"group failed at " + list, 1}, form{list, 0}))
	case c.returnsTo != nil:
		// The bare offset is padded to 4 cells where it fits, so the offsets of a column line up.
		return short(grey, one(form{fmt.Sprintf("returns to %d", *c.returnsTo), 1}, form{fmt.Sprintf("%4d", *c.returnsTo), 1}, form{fmt.Sprint(*c.returnsTo), 0}))
	case c.confirm != nil:
		n := c.confirm
		return short(textStyle, one(
			form{fmt.Sprintf("%d: light %d/%d, heavy %d/%d", n.offset, n.light, n.needed, n.heavy, n.needed), 1},
			form{fmt.Sprintf("L%d/%d H%d/%d", n.light, n.needed, n.heavy, n.needed), 0}))
	case c.holder != nil && c.holder.combination > 0:
		label := fmt.Sprintf("C%d", c.holder.combination)
		clauses := []clause{one(form{"held by " + label, 2}, form{label, 0})}
		if c.solo != nil {
			clauses = append(clauses, one(form{fmt.Sprintf("solo %d", *c.solo), 1}, form{"", 0}))
		}
		return short(magenta, clauses...)
	case c.gaveBack > 0:
		label := fmt.Sprintf("C%d", c.gaveBack)
		clauses := []clause{one(form{"backed off after " + label, 2}, form{label, 0})}
		if c.solo != nil {
			clauses = append(clauses, one(form{fmt.Sprintf("solo %d", *c.solo), 1}, form{"", 0}))
		}
		return short(grey, clauses...)
	case c.state == coreFound && c.solo != nil:
		return short(grey, one(form{fmt.Sprintf("solo %d", *c.solo), 1}, form{fmt.Sprint(*c.solo), 0}))
	case c.state == coreSearch:
		var clauses []clause
		if c.pass != nil {
			pass := one(form{fmt.Sprintf("passed %d", *c.pass), 4}, form{fmt.Sprintf("p %d", *c.pass), 2})
			if c.next != nil {
				pass.forms = append(pass.forms, form{})
			}
			clauses = append(clauses, pass)
		}
		if c.next != nil {
			clauses = append(clauses, one(form{fmt.Sprintf("next %d", *c.next), 3}, form{fmt.Sprintf("→ %d", *c.next), 0}))
		}
		return short(grey, clauses...)
	case c.queued != "":
		return grey.Render(cutWords(vtText(c.queued), width))
	}
	return ""
}

// ccdRole says what this trial does with a CCD's cores, in full and in a word.
func (s Snapshot) ccdRole(id int) (string, string) {
	loaded, judged, parked, total, suspects, members, probes := 0, 0, 0, 0, 0, 0, 0
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
		case coreMember:
			members++
		case coreProbe:
			members++
			probes++
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
	case parked == total && loaded > 0:
		return fmt.Sprintf("%s, %d under load", parkedWords, loaded), "parked"
	case parked == total:
		return parkedWords + ", idle", "parked"
	case loaded == total && suspects == total:
		return "suspects at their failing offsets", "suspects"
	case loaded == total && members == total && probes > 0:
		return "members, one probed shallower than its failing offset", "members"
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

// historyPanel lists what happened in height rows. Its last row counts the entries it leaves out, those that do not
// fit and those older than the entries the snapshot keeps.
func (s Snapshot) historyPanel(width, height int) []string {
	if width <= 0 || height <= 0 {
		return nil
	}
	room := max(height-2, 0)
	shown := min(len(s.history), room)
	if s.historyDropped > 0 || len(s.history) > room {
		shown = min(len(s.history), max(room-1, 0))
	}
	hidden := s.historyDropped + len(s.history) - shown
	headerRows := min(height, 2)
	if hidden > 0 {
		headerRows = min(height-1, 2)
	}
	rows := headerRows + shown
	if hidden > 0 {
		rows++
	}
	out := make([]string, 0, rows)
	if headerRows > 0 {
		right := fmt.Sprintf("newest first · l: last %d events", logLimit)
		if width < 100 {
			right = fmt.Sprintf("l: last %d", logLimit)
		}
		out = append(out, rule(width, grey.Render("WHAT HAPPENED"), grey.Render(right)))
	}
	if headerRows > 1 {
		out = append(out, "")
	}
	for _, e := range s.history[:shown] {
		before, alarm, after := e.sentenceParts()
		text := textStyle.Render(before) + red.Render(alarm) + textStyle.Render(after)
		out = append(out, grey.Render(wallMinute(e.at))+"  "+tagStyle(e.tag).Render(fmt.Sprintf("%-8s", e.tag))+" "+cutWords(text, max(width-16, 0)))
	}
	if hidden > 0 {
		out = append(out, grey.Render(fmt.Sprintf("+%d more", hidden)))
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

// ibm437 holds the non-ASCII glyphs of IBM437, the character set of the kernel's built-in console fonts.
const ibm437 = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■ ☺☻♥♦♣♠•◘○◙♂♀♪♫☼►◄↕‼¶§▬↨↑↓→←∟↔▲▼"

// consoleText replaces what the console font cannot draw: common typography by ASCII, anything else by a visible escape.
func consoleText(text string) string {
	var b strings.Builder
	start := 0
	for i, ch := range text {
		if ch < 128 || strings.ContainsRune(ibm437, ch) {
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

// cutMarker ends text cut to fit; the console font has no "…".
const cutMarker = "..."

// form is one way a clause reads. rank is how early the clause leaves this form for the next one when the line
// does not fit: the higher rank goes first, and of equal ranks the later clause.
type form struct {
	text string
	rank int
}

// clause is a part of a line that shows whole in one of its forms, from longest to shortest; an empty last form lets
// the line leave the clause out, and a value has none. sep goes before the clause unless it opens the line.
type clause struct {
	sep   string
	forms []form
}

// whole is a clause with one form, never shortened or left out.
func whole(sep, text string) clause { return clause{sep, []form{{text, 0}}} }

// fitClauses joins clauses into width cells as the console will show them, shortening or leaving out the clauses
// the caller ranks lowest until the line fits; once none has another form, cutWords cuts the line.
func fitClauses(width int, clauses ...clause) string {
	at := make([]int, len(clauses))
	for {
		var b strings.Builder
		for i, c := range clauses {
			text := c.forms[at[i]].text
			if text == "" {
				continue
			}
			if b.Len() > 0 {
				b.WriteString(c.sep)
			}
			b.WriteString(text)
		}
		line := b.String()
		next := -1
		for i, c := range clauses {
			if at[i]+1 < len(c.forms) && (next < 0 || c.forms[at[i]].rank >= clauses[next].forms[at[next]].rank) {
				next = i
			}
		}
		if next < 0 || ansi.StringWidth(consoleText(line)) <= max(width, 0) {
			return cutWords(line, width)
		}
		at[next]++
	}
}

// cutWords fits text to width cells as the console will show it, after consoleText escapes glyphs it cannot draw.
// Text that does not fit keeps the whole words that fit and ends with cutMarker, or is left out when no word fits.
func cutWords(text string, width int) string {
	if width <= 0 {
		return ""
	}
	text = consoleText(text)
	if ansi.StringWidth(text) <= width {
		return text
	}
	room := width - len(cutMarker)
	if room < 0 {
		return ""
	}
	plain := ansi.Strip(text)
	kept := ansi.Strip(ansi.Truncate(text, room, ""))
	if !strings.HasPrefix(plain[len(kept):], " ") {
		kept = kept[:max(strings.LastIndexByte(kept, ' '), 0)]
	}
	kept = strings.TrimRight(kept, " ·→,;:")
	if kept == "" {
		return ""
	}
	return ansi.Truncate(text, ansi.StringWidth(kept)+len(cutMarker), cutMarker)
}

// wrapStyled wraps text as the console will show it, after consoleText escapes glyphs it cannot draw.
func wrapStyled(text string, width int, style lipgloss.Style) []string {
	if width <= 0 {
		return nil
	}
	text = consoleText(text)
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

// Every time on screen is local, like the header clock and `togi events`: the journal stores UTC, so each
// journal time passes through these two formats and nothing else formats a time.
func wallSecond(t time.Time) string {
	return t.Local().Format("15:04:05")
}

func wallMinute(t time.Time) string {
	return t.Local().Format("15:04")
}
