package watch

import (
	"fmt"
	"slices"
	"strings"
	"time"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/shgew/togi/internal/journal"
)

var (
	colSearch = lipgloss.Color("12")
	colDone   = lipgloss.Color("10")
	colJoint  = lipgloss.Color("11")
	colFail   = lipgloss.Color("9")
	colTrial  = lipgloss.Color("15")
	colDim    = lipgloss.Color("8")

	plain       = lipgloss.NewStyle()
	bold        = plain.Bold(true)
	dim         = plain.Foreground(colDim)
	searchStyle = plain.Foreground(colSearch)
	jointStyle  = plain.Foreground(colJoint)
	failStyle   = plain.Foreground(colFail)
	trialStyle  = plain.Foreground(colTrial).Bold(true)
	digitStyle  = trialStyle
	barStyle    = plain.Foreground(colTrial)
	trialMark   = plain.Foreground(lipgloss.Color("0")).Background(colTrial).Bold(true)
	bannerStyle = plain.Foreground(colTrial).Background(colFail).Bold(true)
)

func phaseColor(p journal.Phase) lipgloss.Style {
	switch p {
	case journal.PhaseDone:
		return plain.Foreground(colDone)
	case journal.PhaseResident:
		return plain.Foreground(colJoint)
	case journal.PhaseSearch, journal.PhaseGuard, journal.PhaseHunt, journal.PhaseRefine:
	}
	return searchStyle
}

type mode int

const (
	roomy mode = iota
	medium
	compact
)

type layout struct {
	margin, gap int
	widths      []int
}

func columns(w int) layout {
	l := layout{gap: 1}
	if w >= 160 {
		l.margin, l.gap = 1, 2
	}
	usable := w - 1 - l.margin
	cols := 1
	for _, n := range []int{8, 4, 2} {
		least := 29
		if n == 8 {
			least = 55
		}
		if (usable-(n-1)*l.gap)/n >= least {
			cols = n
			break
		}
	}
	space := usable - (cols-1)*l.gap
	for i := range cols {
		wi := space / cols
		if i < space%cols {
			wi++
		}
		l.widths = append(l.widths, wi)
	}
	return l
}

// frame collects the lines of one frame; air adds blank lines that only roomy mode keeps in full.
type frame struct {
	lines []string
	mode  mode
}

func (f *frame) add(ls ...string) {
	for _, l := range ls {
		f.lines = append(f.lines, strings.Split(l, "\n")...)
	}
}

func (f *frame) air(roomyLines, otherLines int) {
	n := otherLines
	if f.mode == roomy {
		n = roomyLines
	}
	for range n {
		f.lines = append(f.lines, "")
	}
}

const minWidth = 20

// Render draws the frame for a w by h screen: exactly h-1 lines, each at most w-1 cells wide, so the last row and
// column are never written and the screen never scrolls. Screens narrower than minWidth get a cut-off frame.
func Render(s Snapshot, w, h int, now time.Time) string {
	screen := max(w-1, 0)
	w = max(w, minWidth)
	width := w - 1
	l := columns(w)
	pad := strings.Repeat(" ", l.margin)
	var f frame
	if !s.session {
		f.add(spread(width, pad+bold.Reverse(true).Render(" togi "), bold.Render(now.Format("15:04:05"))), "")
		if s.problem != nil {
			f.add(bannerStyle.Width(width).Render(pad + journal.EscapeText(s.problem.Error())))
		} else {
			f.add(pad + dim.Render("no session yet"))
		}
		return fit(f.lines, screen, h)
	}
	drawn := map[drawnTile][]string{}
	for _, m := range []mode{roomy, medium, compact} {
		f = frame{mode: m}
		s.top(&f, width, pad, now)
		s.board(&f, l, drawn)
		f.add(pad + s.legend())
		if len(f.lines)+3 <= h-1 {
			break
		}
	}
	f.air(2, 0)
	f.add(pad + bold.Render("Recent"))
	room := max(0, h-1-len(f.lines))
	for _, e := range s.recent[max(0, len(s.recent)-room):] {
		f.add(pad + dim.Render(hm(e.at.Sub(s.start))) + "  " + journal.EscapeText(e.msg))
	}
	return fit(f.lines, screen, h)
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

func (s Snapshot) top(f *frame, width int, pad string, now time.Time) {
	stage := "SEARCH"
	switch {
	case s.huntID != 0:
		stage = "HUNT"
	case s.round != 0:
		stage = "REFINE"
	case s.guard:
		stage = "GUARD"
	}
	head := bold.Reverse(true).Render(" togi ") + "  " + bold.Render(stage) + "   " +
		dim.Render("session "+age(now.Sub(s.start))+"   last event "+age(now.Sub(s.last))+" ago")
	f.add(spread(width, pad+head, bold.Render(now.Format("15:04:05"))))
	f.add(pad + dim.Render(strings.Join(s.summary(), " | ")))
	f.air(1, 0)
	f.add(pad + s.trialLine(now))
	if f.mode == roomy {
		f.add(pad + s.scheduleLine())
	}
	switch {
	case s.deadEnd != "":
		f.add(bannerStyle.Width(width).Render(pad + journal.EscapeText(s.deadEnd)))
	case s.lastFailure != nil:
		f.add(pad + dim.Render("last failure "+age(now.Sub(s.lastFailure.at))+" ago   "+journal.EscapeText(s.lastFailure.msg)))
	default:
		f.add(pad + dim.Render("no failures yet"))
	}
	f.air(2, 1)
}

func (s Snapshot) summary() []string {
	done := 0
	for _, t := range s.tiles {
		if t.phase == journal.PhaseDone {
			done++
		}
	}
	var parts []string
	if s.huntID != 0 {
		hunt := fmt.Sprintf("hunt %d", s.huntID)
		if s.maskID != 0 {
			hunt += fmt.Sprintf(" mask %d", s.maskID)
		}
		parts = append(parts, hunt)
	}
	if s.round != 0 {
		parts = append(parts, fmt.Sprintf("refine round %d", s.round))
	}
	parts = append(parts, fmt.Sprintf("%d/%d done", done, len(s.tiles)))
	if gs := s.guardState; gs != nil {
		parts = append(parts, fmt.Sprintf("qualified rotations %d since last deepening, latest %d", gs.CleanRotations, gs.LastQualifiedRotation))
		if gs.TctlMaxC != nil {
			parts = append(parts, fmt.Sprintf("Tctl profile max %d C", *gs.TctlMaxC))
		}
	}
	parts = append(parts, plural(s.failures, "failure"), plural(s.crashes, "crash"))
	if s.tctlTrial != nil && !s.guard {
		parts = append(parts, fmt.Sprintf("Tctl last trial max %d C", *s.tctlTrial))
	}
	return parts
}

func (s Snapshot) trialLine(now time.Time) string {
	t := s.trial
	if t == nil {
		if s.inFlight != "" {
			return dim.Render(journal.EscapeText(s.inFlight))
		}
		return dim.Render("no trial running")
	}
	var target string
	switch {
	case t.all:
		target = fmt.Sprintf("all %d cores", len(t.cores))
	case len(t.cores) == 1:
		target = fmt.Sprintf("core %02d", t.cores[0])
	default:
		ids := make([]string, len(t.cores))
		for i, c := range t.cores {
			ids[i] = fmt.Sprintf("%02d", c)
		}
		target = "cores " + strings.Join(ids, " ")
	}
	var elapsed time.Duration
	if t.hasStarted {
		elapsed = min(max(now.Sub(t.started), 0), t.duration)
	}
	const cells = 30
	n := 0
	if t.duration > 0 {
		n = int(cells * elapsed / t.duration)
	}
	bar := barStyle.Render(strings.Repeat("█", n)) + dim.Render(strings.Repeat("░", cells-n)) +
		fmt.Sprintf(" %d/%ds", int(elapsed.Seconds()), int(t.duration.Seconds()))
	return bold.Render(target) + "   " + journal.EscapeText(string(t.condition)) + "   " + journal.EscapeText(string(t.regime)) + " " + journal.EscapeText(t.workload) + "   " + bar
}

func (s Snapshot) scheduleLine() string {
	if !s.guard {
		return s.turnOrder()
	}
	gs := s.guardState
	if gs == nil {
		return ""
	}
	steps := func(rs []string) string { return strings.Join(rs, " ") }
	names := make([]string, len(gs.Steps))
	for i, r := range gs.Steps {
		names[i] = journal.EscapeText(string(r))
	}
	var out string
	if m := len(names); gs.RotationOpen && gs.StepsDone < m {
		cur := gs.StepsDone
		out = dim.Render(fmt.Sprintf("rotation %d   step %d/%d   ", gs.Rotation, cur+1, m))
		if cur > 0 {
			out += dim.Render(steps(names[:cur])) + dim.Render(" | ")
		}
		out += bold.Render(names[cur])
		if cur+1 < m {
			out += dim.Render(" | ") + dim.Render(steps(names[cur+1:]))
		}
	} else {
		out = dim.Render(fmt.Sprintf("rotation %d done   ", gs.Rotation) + steps(names))
	}
	if !gs.Qualifying && len(gs.Missing) > 0 {
		out += dim.Render("   not qualifying: " + journal.EscapeText(strings.Join(gs.Missing, "; ")))
	}
	return out
}

func (s Snapshot) turnOrder() string {
	ids := make([]string, len(s.order))
	for i, id := range s.order {
		style := dim
		if id == s.current {
			style = bold
		}
		ids[i] = style.Render(fmt.Sprintf("%02d", id))
	}
	out := dim.Render("turn order ") + strings.Join(ids, " ")
	start := 0
	if i := slices.Index(s.order, s.current); i >= 0 {
		start = i + 1
	}
	var next []string
	for k := range s.order {
		id := s.order[(start+k)%len(s.order)]
		if id == s.current || len(next) == 3 {
			continue
		}
		if i := slices.IndexFunc(s.tiles, func(t tile) bool { return t.id == id }); i >= 0 {
			if p := s.tiles[i].phase; p == journal.PhaseSearch {
				next = append(next, fmt.Sprintf("%02d", id))
			}
		}
	}
	if len(next) > 0 {
		out += dim.Render("   up next " + strings.Join(next, ", "))
	}
	return out
}

// drawnTile identifies a tile's rendered lines, which Render reuses across the modes it tries.
type drawnTile struct {
	tile, width int
	compact     bool
}

func (s Snapshot) board(f *frame, l layout, drawn map[drawnTile][]string) {
	pad := strings.Repeat(" ", l.margin)
	var ccds []int
	for _, t := range s.tiles {
		if !slices.Contains(ccds, t.ccd) {
			ccds = append(ccds, t.ccd)
		}
	}
	slices.Sort(ccds)
	cols := len(l.widths)
	for ci, ccd := range ccds {
		f.add(pad + bold.Render(fmt.Sprintf("CCD %d", ccd)))
		f.air(1, 0)
		var group []int
		for i, t := range s.tiles {
			if t.ccd == ccd {
				group = append(group, i)
			}
		}
		for r := 0; r < len(group); r += cols {
			var lines []string
			for i, ti := range group[r:min(r+cols, len(group))] {
				key := drawnTile{tile: ti, width: l.widths[i], compact: f.mode == compact}
				tl, ok := drawn[key]
				if !ok {
					tl = s.tiles[ti].render(key.width, key.compact)
					drawn[key] = tl
				}
				for j, ln := range tl {
					if i == 0 {
						lines = append(lines, pad+ln)
					} else {
						lines[j] += strings.Repeat(" ", l.gap) + ln
					}
				}
			}
			f.add(lines...)
			if r+cols < len(group) {
				f.air(1, 0)
			}
		}
		if ci+1 < len(ccds) {
			f.air(2, 1)
		} else {
			f.air(2, 0)
		}
	}
}

func (s Snapshot) legend() string {
	return dim.Render(strings.Join([]string{
		"CO 0 -> -50",
		trialStyle.Render(">") + " trial",
		failStyle.Render("x") + " failed mark",
		jointStyle.Render("j") + " joint mark",
		dim.Render("·") + " mask anchor",
		searchStyle.Render("░") + " unexplored",
	}, "     "))
}

func spread(width int, left, right string) string {
	return left + strings.Repeat(" ", max(1, width-lipgloss.Width(left)-lipgloss.Width(right))) + right
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
