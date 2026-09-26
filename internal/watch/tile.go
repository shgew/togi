package watch

import (
	"fmt"
	"strings"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"

	"code.marleb.org/shgew/shycler/internal/journal"
)

type cellKind int

const (
	emptyCell cellKind = iota
	filledCell
	unexploredCell
	tryingCell
	settledCell
	regainCell
	failCell
)

func (t tile) searching() bool {
	return t.phase == journal.PhaseSearch || t.phase == ""
}

func (t tile) confirmed() bool {
	return t.phase == journal.PhaseConfirmed || t.phase == journal.PhaseGuard
}

// kindAt is what depth d shows; markers is false in the cells after a depth's first, where the failed mark and the
// trial marker are not repeated.
func (t tile) kindAt(d int, markers bool) cellKind {
	e := -t.number
	switch {
	case markers && t.fail != nil && d == -*t.fail:
		return failCell
	case markers && t.trying != nil && d == -*t.trying:
		return tryingCell
	case t.confirmed() && d > e && d <= e+t.regain:
		return regainCell
	case t.confirmed() && d > e+t.regain && d <= e+t.regain+t.settled:
		return settledCell
	case t.hasNumber && d <= e:
		return filledCell
	case t.searching() && t.hasNumber && t.fail != nil && d > e && d < -*t.fail:
		return unexploredCell
	}
	return emptyCell
}

// span is the depth range cell i of n covers; with n > 50 neighbouring cells share a depth.
func span(i, n int) (lo, hi int) {
	lo = i*50/n + 1
	return lo, max(lo, (i+1)*50/n)
}

// cellOf is the first of n cells covering depth d.
func cellOf(d, n int) int {
	for i := range n {
		if _, hi := span(i, n); hi >= d {
			return i
		}
	}
	return n - 1
}

func (t tile) gauge(n int) string {
	kinds := make([]cellKind, n)
	for i := range n {
		lo, hi := span(i, n)
		for d := lo; d <= hi; d++ {
			kinds[i] = max(kinds[i], t.kindAt(d, cellOf(d, n) == i))
		}
	}
	var b strings.Builder
	for i := 0; i < n; {
		j := i + 1
		for j < n && kinds[j] == kinds[i] {
			j++
		}
		style, glyph := t.cellLook(kinds[i])
		b.WriteString(style.Render(strings.Repeat(glyph, j-i)))
		i = j
	}
	return b.String()
}

func (t tile) cellLook(k cellKind) (lipgloss.Style, string) {
	switch k {
	case failCell:
		return failStyle, "x"
	case regainCell:
		return regainStyle, "▒"
	case settledCell:
		return dim, ":"
	case tryingCell:
		return trialStyle, ">"
	case unexploredCell:
		return searchStyle, "░"
	case filledCell:
		return phaseColor(t.phase), "█"
	case emptyCell:
		return dim, "."
	}
	return dim, "."
}

type label struct {
	depth int
	text  string
	style lipgloss.Style
}

// axis writes values under the gauge cells they describe; a label moves up to two cells aside to avoid touching
// another, and end labels give way to values.
func axis(labels []label, inner int) string {
	slots := make([]string, inner)
	for i := range slots {
		slots[i] = " "
	}
	used := make([]bool, inner)
	for _, l := range labels {
		n := len(l.text)
		ideal := min(max(0, cellOf(l.depth, inner)-n/2), inner-n)
		for _, shift := range []int{0, 1, -1, 2, -2} {
			at := ideal + shift
			if at < 0 || at+n > inner {
				continue
			}
			free := true
			for i := max(0, at-1); i < min(inner, at+n+1); i++ {
				free = free && !used[i]
			}
			if !free {
				continue
			}
			for i, r := range l.text {
				slots[at+i] = l.style.Render(string(r))
				used[at+i] = true
			}
			break
		}
	}
	return strings.Join(slots, "")
}

func (t tile) word() string {
	switch t.phase {
	case journal.PhaseConfirmation:
		return fmt.Sprintf("confirming %d/%d", t.slots, t.total)
	case journal.PhaseConfirmed, journal.PhaseGuard:
		return "confirmed"
	case journal.PhaseSearch:
	}
	return "searching"
}

func (t tile) topEdge(width int, border lipgloss.Style) string {
	right := " " + phaseColor(t.phase).Render(t.word()) + border.Render(" ─┐")
	var left string
	for _, short := range []bool{false, true} {
		left = border.Render("┌─ ") + bold.Render(fmt.Sprintf("%02d", t.id))
		if t.loaded {
			left += " " + trialMark.Render(pick(short, "> TESTING", ">"))
		}
		if t.regain > 0 {
			left += " " + regainStyle.Bold(true).Render(pick(short, "~ BACKOFF", "~"))
		} else if t.settled > 0 {
			left += " " + dim.Render(pick(short, "SETTLED", ":"))
		}
		if width-lipgloss.Width(left)-lipgloss.Width(right)-1 >= 1 {
			break
		}
	}
	if width-lipgloss.Width(left)-lipgloss.Width(right)-1 < 1 {
		right = border.Render("─┐")
		left = ansi.Truncate(left, width-lipgloss.Width(right)-2, "")
	}
	fill := width - lipgloss.Width(left) - lipgloss.Width(right) - 1
	return left + " " + border.Render(strings.Repeat("─", max(1, fill))) + right
}

func pick(short bool, long, brief string) string {
	if short {
		return brief
	}
	return long
}

func (t tile) render(width int, m mode) []string {
	border := phaseColor(t.phase)
	if t.regain > 0 {
		border = regainStyle
	}
	inner := width - 4
	num := "--"
	if t.hasNumber {
		num = fmt.Sprint(t.number)
	}
	var labels []label
	if t.trying != nil {
		labels = append(labels, label{-*t.trying, fmt.Sprint(*t.trying), trialStyle})
	}
	if t.hasNumber && (t.regain > 0 || t.settled > 0) {
		labels = append(labels, label{-t.number, num, phaseColor(t.phase).Bold(true)})
	}
	if t.fail != nil {
		labels = append(labels, label{-*t.fail, fmt.Sprint(*t.fail), failStyle})
	}
	labels = append(labels, label{1, "0", dim}, label{50, "-50", dim})

	var rows []string
	if m == compact {
		rows = append(rows, bold.Render(fmt.Sprintf("%3s", num)))
	} else {
		for _, d := range bigDigits(num) {
			rows = append(rows, lipgloss.PlaceHorizontal(inner, lipgloss.Center, digitStyle.Render(d)))
		}
	}
	rows = append(rows, t.gauge(inner), axis(labels, inner))
	side := border.Render("│")
	lines := []string{t.topEdge(width, border)}
	for _, r := range rows {
		r = ansi.Truncate(r, inner, "")
		lines = append(lines, side+" "+r+strings.Repeat(" ", inner-ansi.StringWidth(r))+" "+side)
	}
	lines = append(lines, border.Render("└"+strings.Repeat("─", width-2)+"┘"))
	return lines
}

var font = map[rune][3]string{
	'0': {"█▀█", "█ █", "▀▀▀"}, '1': {"▀█ ", " █ ", "▀▀▀"}, '2': {"▀▀█", "█▀▀", "▀▀▀"},
	'3': {"▀▀█", " ▀█", "▀▀▀"}, '4': {"█ █", "▀▀█", "  ▀"}, '5': {"█▀▀", "▀▀█", "▀▀▀"},
	'6': {"█▀▀", "█▀█", "▀▀▀"}, '7': {"▀▀█", "  █", "  ▀"}, '8': {"█▀█", "█▀█", "▀▀▀"},
	'9': {"█▀█", "▀▀█", "▀▀▀"}, '-': {"   ", "▀▀▀", "   "}, ' ': {"   ", "   ", "   "},
}

func bigDigits(num string) [3]string {
	var rows [3]string
	for i, r := range fmt.Sprintf("%3s", num) {
		for row := range 3 {
			if i > 0 {
				rows[row] += " "
			}
			rows[row] += font[r][row]
		}
	}
	return rows
}
