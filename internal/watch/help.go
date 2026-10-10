package watch

import (
	"fmt"
	"strings"

	"github.com/charmbracelet/x/ansi"
)

type helpSection struct {
	title string
	items []helpItem
}

type helpItem struct {
	label, text string
}

var screenHelp = helpSection{"READING THE SCREEN", []helpItem{
	{"►", "the core carries load in this trial"},
	{"▄▄▄", "offset applied now, or saved after a stop restored the hardware; white when this trial judges the core"},
	{"grey ▄▄▄", "the same depth, grey when the core is not judged"},
	{"···", "depth it reached but does not use now: given back, parked, waiting"},
	{"░░░", "depth a combination blocks now"},
	{"█", "its failure point: this core failed there on its own account"},
	{"▀", "a group trial failed with this core at that offset"},
	{"cyan", "running now"},
	{"amber", "a hunt, and what it paused"},
	{"red", "failures, past and present"},
	{"green", "passed, done"},
	{"magenta", "combinations"},
	{"grey", "idle, parked, still to come"},
}}

var topHelp = helpSection{"THE TOP LINE", []helpItem{
	{"failures", "failure decisions, including trials skipped because they already failed"},
	{"crashes", "boots that ended without a clean shutdown"},
	{"last observed", "the last failure seen in this session that changed what togi does; record-only results and skips do not count"},
	{"q", "closes this view only; tuning keeps running"},
}}

var tuningHelp = helpSection{"WHAT TOGI DOES", []helpItem{
	{"SOLO LIMITS", "Search: each core alone, the rest at 0, finds its solo limit. The stage counts the cores whose solo limit is found and names the core being searched or confirmed."},
	{"CYCLES", "Checking: every core at its offset together, through the steps of a cycle. The stage shows the cycle's number and step; the cycle panel lists its steps and R7 parts."},
	{"HUNT", "An amber detour: parked trials that find the core or combination behind a failure. The hunt panel shows its groups and member probes."},
	{"DEEPEN", "Deepening: rounds that move the profile deeper, as far as its failure points and combinations permit. The stage shows the round."},
	{"CLEAN CYCLES", "The count of clean cycles still valid for the current profile; checking repeats cycles until stopped."},
	{"R6", "During idle trials this screen holds still, clock included, so it cannot wake the cores."},
	{"NO ETA", "A hunt can start at any time, so togi shows only what is scheduled and the time left in the current trial."},
	{"RULES", "How togi decides what to do: https://github.com/shgew/togi/blob/main/docs/spec/tuner.md"},
}}

var wordsHelp = helpSection{"WORDS", []helpItem{
	{"SEARCH", "finding its solo limit with light and heavy-vector trials alone"},
	{"CONFIRM", "checking its candidate solo limit with repeated light and heavy-vector trials alone"},
	{"FOUND", "solo limit checked; waiting at 0 until every core has one"},
	{"WAITING", "at 0, waiting for its first search turn"},
	{"MEMBER", "kept with its hunt group at its failing offset; another member may be probed"},
	{"offset", "Curve Optimizer counts, 0 to -50; deeper is more negative"},
	{"trial", "one launch of one workload on its target"},
	{"solo limit", "the deepest offset a core confirmed under load alone"},
	{"failure point", "the shallowest offset where a core failed on its own account"},
	{"combination", "offsets at which a group of cores failed together. Unresolved: no smaller group was shown to fail"},
	{"at its limit", "a core at -50, or one count deeper reaches a failure point or combination"},
	{"has room", "a core past search that is not at its limit"},
	{"suspect", "a core a hunt keeps at its failing offset"},
	{"parked", "a core a hunt holds shallower than its failing offset"},
	{"partial part", "an R7 part with the top requesters idle"},
	{"top requester", "the loaded core with the highest voltage request, setting the shared core voltage; marked top on the CCD tables, by offset when offsets stand in for requests"},
	{"self-sufficient", "observed passing as top requester for this R7 workload; evidence, not a guarantee"},
	{"record only", "a trial whose result is kept but changes no decision"},
	{"cycle", "one pass through the checking schedule"},
}}

var loadHelp = helpSection{"KINDS OF LOAD", []helpItem{
	{"R1 light", "one light thread: compiling, browsing"},
	{"R2 heavy vector", "one AVX2/AVX-512 thread: encoding, rendering"},
	{"R3 load steps", "load switching on and off: current steps"},
	{"R4 partial load", "light load at 25-75% duty: bursty work"},
	{"R5 both threads", "both threads of one core: SMT pressure"},
	{"R6 idle + bursts", "idle with short wake-ups: idle and boost"},
	{"R7 all-core", "a CCD, part of one, or every core: power and heat"},
}}

func renderHelpBody(width, height, scroll int) ([]string, int) {
	bodyWidth := max(1, width-2)
	if width >= 195 {
		leftWidth := (bodyWidth - 8) * 81 / 227
		middleWidth := (bodyWidth - 8) * 75 / 227
		rightWidth := bodyWidth - 8 - leftWidth - middleWidth
		columns := [][]string{
			append(append(helpReading(leftWidth), "", ""), helpSectionLines(topHelp, leftWidth, false)...),
			helpSectionLines(tuningHelp, middleWidth, true),
			append(append(helpSectionLines(wordsHelp, rightWidth, false), "", ""), helpSectionLines(loadHelp, rightWidth, false)...),
		}
		rows := max(len(columns[0]), len(columns[1]), len(columns[2]))
		lines := make([]string, rows)
		for row := range rows {
			for col, colWidth := range []int{leftWidth, middleWidth, rightWidth} {
				line := ""
				if row < len(columns[col]) {
					line = columns[col][row]
				}
				lines[row] += line + strings.Repeat(" ", max(0, colWidth-ansi.StringWidth(line)))
				if col < 2 {
					lines[row] += "    "
				}
			}
		}
		return scrollBodyWithBar(lines, width, height, scroll)
	}
	lines := helpReading(bodyWidth)
	for _, section := range []helpSection{topHelp, tuningHelp, wordsHelp, loadHelp} {
		lines = append(lines, "", "")
		lines = append(lines, helpSectionLines(section, bodyWidth, section.title == tuningHelp.title)...)
	}
	return scrollBodyWithBar(lines, width, height, scroll)
}

func helpReading(width int) []string {
	out := []string{helpRule(screenHelp.title, width), ""}
	for _, example := range []struct {
		core, offset, failure int
		state, note           string
		gauge                 string
	}{
		{4, -28, -36, "AT LIMIT", "at its limit: one count deeper reaches C5", white.Render(strings.Repeat("▄", 28)) + magenta.Render(strings.Repeat("░", 7)) + red.Render("█")},
		{0, -26, -32, "HAS ROOM", "has room: backed off after a hunt, may deepen", white.Render(strings.Repeat("▄", 26)) + grey.Render("·····") + red.Render("█")},
		{9, 0, -48, "PARKED", "parked at 0 by a hunt; returns to -47", grey.Render(strings.Repeat("·", 47)) + red.Render("█")},
		{0, -26, -32, "PROBE", "probe: the group failed with core 00 at -30, -29, -27", white.Render(strings.Repeat("▄", 26)) + red.Render("▀ ▀▀ █")},
	} {
		marker := "► "
		if example.state == "PARKED" {
			marker = "  "
		}
		prefix := fmt.Sprintf("%s%02d  %3d  %-8s ", marker, example.core, example.offset, example.state)
		gauge := example.gauge + strings.Repeat(" ", max(0, 50-ansi.StringWidth(example.gauge)))
		failure := fmt.Sprintf("%3d", example.failure)
		out = append(out, helpExampleRow(width, []string{prefix, fmt.Sprintf("%s%02d  %3d ", marker, example.core, example.offset), fmt.Sprintf("%s%02d ", marker, example.core), ""}, gauge, failure))
		indent := min(ansi.StringWidth(prefix), max(0, width-20))
		for _, line := range wrapStyled(example.note, max(1, width-indent), grey) {
			out = append(out, strings.Repeat(" ", indent)+line)
		}
		out = append(out, "")
	}
	items := helpSectionLines(screenHelp, width, false)
	out = append(out, items[2:]...)
	return out
}

// helpExampleRow keeps the text of a help example whole: its heads run from the full core, offset and state to
// nothing, each with its failure point or without, and the first that fits wins. Only the gauge is clipped by cells.
func helpExampleRow(width int, heads []string, gauge, failure string) string {
	for _, head := range heads {
		for _, withFailure := range []bool{true, false} {
			used := ansi.StringWidth(head)
			if withFailure {
				used += 2 + ansi.StringWidth(failure)
			}
			if used > width {
				continue
			}
			line := gauge
			if head != "" {
				line = white.Render(head) + line
			}
			line = ansi.Truncate(line, width-used+ansi.StringWidth(head), "")
			if withFailure {
				line += "  " + red.Render(failure)
			}
			return line
		}
	}
	return ""
}

func helpSectionLines(section helpSection, width int, spaced bool) []string {
	out := []string{helpRule(section.title, width), ""}
	labelWidth := 0
	for _, item := range section.items {
		labelWidth = max(labelWidth, ansi.StringWidth(item.label))
	}
	labelWidth = min(labelWidth+2, max(1, width/3))
	for i, item := range section.items {
		text := wrapStyled(item.text, max(1, width-labelWidth), textStyle)
		for row, line := range text {
			label := strings.Repeat(" ", labelWidth)
			if row == 0 {
				name := cutWords(item.label, labelWidth-1)
				label = white.Render(name) + strings.Repeat(" ", labelWidth-ansi.StringWidth(name))
			}
			out = append(out, ansi.Truncate(label+line, width, ""))
		}
		if spaced && i < len(section.items)-1 {
			out = append(out, "")
		}
	}
	return out
}

func helpRule(title string, width int) string {
	width = max(width, 0)
	title = cutWords(title, width)
	if ansi.StringWidth(title) == width {
		return white.Render(title)
	}
	return white.Render(title) + " " + grey.Render(strings.Repeat("─", max(0, width-ansi.StringWidth(title)-1)))
}

// heldLog is the journal view's list while it is scrolled back: the entries it held when the reader left the end.
// Events that arrive later stay out of it, so the rows the reader is on do not move.
type heldLog struct {
	held  bool
	log   []entry
	total int
}

// apply gives s the held list while sc is the journal view scrolled back, counting the events that arrived since, and
// lets go of it when the view follows the end again or closes.
func (h *heldLog) apply(s Snapshot, sc Screen) Snapshot {
	if sc.View != LogView || sc.Scroll < 0 {
		*h = heldLog{}
		return s
	}
	if !h.held || s.logTotal < h.total {
		*h = heldLog{held: true, log: s.log, total: s.logTotal}
	}
	s.log, s.logTotal, s.logArrived = h.log, h.total, s.logTotal-h.total
	return s
}

// logRule heads the journal view and says what its list leaves out: events before the newest logLimit, and SMU and
// preflight events, which the journal view never lists.
func (s Snapshot) logRule(width int) string {
	notes := []string{"SMU and preflight events left out"}
	if s.logTotal > logLimit {
		notes = []string{
			fmt.Sprintf("last %d of %d events · SMU and preflight left out", logLimit, s.logTotal),
			fmt.Sprintf("last %d of %d events", logLimit, s.logTotal),
		}
	}
	for _, note := range notes {
		if need := ansi.StringWidth("JOURNAL") + ansi.StringWidth(note) + 2; need <= width {
			return helpRule("JOURNAL", width-ansi.StringWidth(note)-1) + " " + grey.Render(note)
		}
	}
	return helpRule("JOURNAL", width)
}

func renderLogBody(s Snapshot, width, height, scroll int) ([]string, int) {
	bodyWidth := max(1, width-2)
	lines := []string{s.logRule(bodyWidth), ""}
	if len(s.log) == 0 {
		lines = append(lines, grey.Render("No journal entries yet."))
	}
	for _, e := range s.log {
		stamp := grey.Render(wallSecond(e.at)) + "  "
		tag := cutWords(vtText(e.tag), 20)
		prefix := stamp + toneStyle(e.tone).Render(fmt.Sprintf("%-20s", tag)) + "  "
		room := max(0, bodyWidth-ansi.StringWidth(prefix))
		text := cutWords(e.text, room)
		lines = append(lines, prefix+textStyle.Render(text))
	}
	return scrollBodyWithBar(lines, width, height, scroll)
}

func scrollBody(lines []string, width, height, scroll int) ([]string, int) {
	height = max(0, height)
	scrolled := max(0, len(lines)-height)
	top := min(max(scroll, 0), scrolled)
	if scroll < 0 {
		top = scrolled
	}
	out := make([]string, 0, height)
	for _, line := range lines[top:min(len(lines), top+height)] {
		out = append(out, ansi.Truncate(line, max(0, width), ""))
	}
	return out, scrolled
}

func scrollBodyWithBar(lines []string, width, height, scroll int) ([]string, int) {
	out, scrolled := scrollBody(lines, max(0, width-2), height, scroll)
	top := min(max(scroll, 0), scrolled)
	if scroll < 0 {
		top = scrolled
	}
	bar := scrollbar(max(0, height), len(lines), top, scrolled)
	for row := range out {
		out[row] = ansi.Truncate(bar[row]+" "+out[row], max(0, width), "")
	}
	return out, scrolled
}
