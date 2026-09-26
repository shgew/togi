package watch

import (
	"fmt"
	"slices"
	"strings"
	"time"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/shgew/shycler/internal/journal"
)

var (
	colSearch  = lipgloss.Color("12")
	colConfirm = lipgloss.Color("13")
	colDone    = lipgloss.Color("10")
	colRegain  = lipgloss.Color("11")
	colFail    = lipgloss.Color("9")
	colTrial   = lipgloss.Color("15")
	colDim     = lipgloss.Color("8")

	plain       = lipgloss.NewStyle()
	bold        = plain.Bold(true)
	dim         = plain.Foreground(colDim)
	searchStyle = plain.Foreground(colSearch)
	regainStyle = plain.Foreground(colRegain)
	failStyle   = plain.Foreground(colFail)
	trialStyle  = plain.Foreground(colTrial).Bold(true)
	digitStyle  = trialStyle
	barStyle    = plain.Foreground(colTrial)
	trialMark   = plain.Foreground(lipgloss.Color("0")).Background(colTrial).Bold(true)
	bannerStyle = plain.Foreground(colTrial).Background(colFail).Bold(true)
)

func phaseColor(p journal.Phase) lipgloss.Style {
	switch p {
	case journal.PhaseConfirmation:
		return plain.Foreground(colConfirm)
	case journal.PhaseConfirmed, journal.PhaseGuard:
		return plain.Foreground(colDone)
	case journal.PhaseSearch:
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
		f.add(spread(width, pad+bold.Reverse(true).Render(" shycler "), bold.Render(now.Format("15:04:05"))), "")
		if s.problem != "" {
			f.add(bannerStyle.Width(width).Render(pad + s.problem))
		} else {
			f.add(pad + dim.Render("no session yet"))
		}
		return fit(f.lines, screen, h)
	}
	for _, m := range []mode{roomy, medium, compact} {
		f = frame{mode: m}
		s.top(&f, width, pad, now)
		s.board(&f, l)
		f.add(pad + s.legend())
		if len(f.lines)+3 <= h-1 {
			break
		}
	}
	f.air(2, 0)
	f.add(pad + bold.Render("Recent"))
	room := max(0, h-1-len(f.lines))
	for _, e := range s.recent[max(0, len(s.recent)-room):] {
		f.add(pad + dim.Render(hm(e.at.Sub(s.start))) + "  " + e.msg)
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
	if s.guard {
		stage = "GUARD"
	}
	head := bold.Reverse(true).Render(" shycler ") + "  " + bold.Render(stage) + "   " +
		dim.Render("session "+age(now.Sub(s.start))+"   last event "+age(now.Sub(s.last))+" ago")
	f.add(spread(width, pad+head, bold.Render(now.Format("15:04:05"))))
	f.add(pad + dim.Render(strings.Join(s.summary(), "   ")))
	f.air(1, 0)
	f.add(pad + s.trialLine(now))
	if f.mode == roomy {
		f.add(pad + s.scheduleLine())
	}
	switch {
	case s.deadEnd != "":
		f.add(bannerStyle.Width(width).Render(pad + s.deadEnd))
	case s.lastFailure != nil:
		f.add(pad + dim.Render("last failure "+age(now.Sub(s.lastFailure.at))+" ago   "+s.lastFailure.msg))
	default:
		f.add(pad + dim.Render("no failures yet"))
	}
	f.air(2, 1)
}

func (s Snapshot) summary() []string {
	var searching, confirming, confirmed, regain, regainCores, settled, settledCores int
	for _, t := range s.tiles {
		switch t.phase {
		case journal.PhaseSearch:
			searching++
		case journal.PhaseConfirmation:
			confirming++
		case journal.PhaseConfirmed, journal.PhaseGuard:
			confirmed++
		}
		if t.regain > 0 {
			regain += t.regain
			regainCores++
		}
		if t.settled > 0 {
			settled += t.settled
			settledCores++
		}
	}
	done := fmt.Sprintf("confirmed %d/%d", confirmed, len(s.tiles))
	if !s.guard {
		parts := []string{fmt.Sprintf("searching %d", searching), fmt.Sprintf("confirming %d", confirming), done,
			plural(s.failures, "failure"), plural(s.crashes, "crash")}
		if s.tctlTrial != nil {
			parts = append(parts, fmt.Sprintf("Tctl last trial max %d C", *s.tctlTrial))
		}
		return parts
	}
	tier := string(s.tier)
	if s.tier == journal.TierNone || s.tier == "" {
		tier = "--"
	}
	parts := []string{done, "tier " + tier}
	gs := s.guardState
	if gs != nil {
		parts = append(parts, fmt.Sprintf("clean %s since profile #%d", hm(time.Duration(gs.CleanS)*time.Second), gs.ProfileSeq))
	}
	if regain > 0 {
		parts = append(parts, fmt.Sprintf("regainable %d on %s", regain, plural(regainCores, "core")))
	}
	if settled > 0 {
		parts = append(parts, fmt.Sprintf("settled %d on %s", settled, plural(settledCores, "core")))
	}
	if gs != nil && gs.TctlMaxC != nil {
		parts = append(parts, fmt.Sprintf("Tctl profile max %d C", *gs.TctlMaxC))
	}
	return parts
}

func (s Snapshot) trialLine(now time.Time) string {
	t := s.trial
	if t == nil {
		if s.inFlight != "" {
			return dim.Render(s.inFlight)
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
	return bold.Render(target) + "   " + string(t.condition) + "   " + string(t.regime) + " " + t.workload + "   " + bar
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
		names[i] = string(r)
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
	if slices.ContainsFunc(s.tiles, func(t tile) bool { return t.regain > 0 }) {
		out += dim.Render("   regain after a clean rotation")
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
			if p := s.tiles[i].phase; p == journal.PhaseSearch || p == journal.PhaseConfirmation {
				next = append(next, fmt.Sprintf("%02d", id))
			}
		}
	}
	if len(next) > 0 {
		out += dim.Render("   up next " + strings.Join(next, ", "))
	}
	return out
}

func (s Snapshot) board(f *frame, l layout) {
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
		var group []tile
		for _, t := range s.tiles {
			if t.ccd == ccd {
				group = append(group, t)
			}
		}
		for r := 0; r < len(group); r += cols {
			var lines []string
			for i, t := range group[r:min(r+cols, len(group))] {
				for j, ln := range t.render(l.widths[i], f.mode) {
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
	parts := []string{"CO 0 -> -50", trialStyle.Render(">") + " trial", failStyle.Render("x") + " failed mark"}
	if s.guard {
		parts = append(parts, regainStyle.Render("▒")+" regainable", dim.Render(":")+" settled")
	} else {
		parts = append(parts, searchStyle.Render("░")+" unexplored")
	}
	return dim.Render(strings.Join(parts, "     "))
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
