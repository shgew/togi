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
	{"SOLO LIMITS", "One core under load, the rest at 0. Step 5 counts deeper while light (R1) and heavy (R2) pass; near a failure, 1 count at a time. Confirm the deepest pass with the configured number of light and heavy trials in a row. A failure while confirming discards that pass and the search backs off."},
	{"CYCLES", "All cores at their offsets, through the configured steps of a cycle. R7 runs each CCD and both, then partial loads idle top-requester groups in request order. The loaded set freezes when the part starts, even if offsets change; passes and failures count as ordinary evidence."},
	{"FAILURES", "Outside multi-core R7, a named failure sets a core's failure point; an unnamed failure starts a hunt unless already recorded. Multi-core R7 does not hunt: named failures count against that core, otherwise against top requesters. Significant failures trigger voltage-targeted backoff; tolerated failures repeat without moving offsets. Record-only results change nothing. A failure with every core at 0 stops tuning."},
	{"HUNT", "Rerun the failing load with a part of the candidates at their failing offsets and the rest parked; keep a part that fails. If no part fails, the group is kept as a combination, and member probes find how far one member must back off for the rest to pass."},
	{"DEEPEN", "After a passed full cycle, aim at the deepest safe total and move each core with room halfway toward it, letting others yield first where they must; every move is checked."},
	{"CLEAN CYCLES", "The goal: a passed full cycle, every step and R7 partial part passed, with every core at its limit and nothing left to deepen. Counted for the current profile; cycles repeat until stopped."},
	{"R6", "During idle trials this screen holds still, clock included, so it cannot wake the cores."},
	{"NO ETA", "A hunt can start at any time, so togi shows only what is scheduled and the time left in the current trial."},
}}

var wordsHelp = helpSection{"WORDS", []helpItem{
	{"offset", "Curve Optimizer counts, 0 to -50; deeper is more negative"},
	{"trial", "one launch of one workload; evidence is counted in trials"},
	{"solo limit", "the deepest offset a core confirmed under load alone"},
	{"failure point", "the shallowest offset where a core failed on its own account"},
	{"combination", "offsets at which a group of cores failed together; the profile is kept from reaching all of them at once. Unresolved: no smaller group was shown to fail"},
	{"at its limit", "-50, or one count deeper reaches a failure point or combination"},
	{"has room", "one count deeper reaches neither; deepening may take it"},
	{"suspect", "a core a hunt keeps at its failing offset"},
	{"parked", "a core a hunt holds shallower: at its last passed full-cycle offset raised to the failing profile, or 0"},
	{"partial part", "R7 with top-requester groups idle, down to two loaded cores; passes and failures count as ordinary evidence"},
	{"top requester", "loaded core with the highest voltage request; both CCDs share it. Within 1 mV ties; offset order is a fallback, not a measurement"},
	{"self-sufficient", "observed passing as top requester for this R7 workload at equal or deeper offsets since reset; evidence, not a guarantee"},
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
	{"R7 all-core", "each CCD, then both: power and heat"},
}}

func renderHelpBody(width, height, scroll int) ([]string, int) {
	if width >= 195 {
		leftWidth := (width - 8) * 81 / 227
		middleWidth := (width - 8) * 75 / 227
		rightWidth := width - 8 - leftWidth - middleWidth
		columns := [][]string{
			append(helpReading(leftWidth), helpSectionLines(topHelp, leftWidth, false)...),
			helpSectionLines(tuningHelp, middleWidth, true),
			append(helpSectionLines(wordsHelp, rightWidth, false), helpSectionLines(loadHelp, rightWidth, false)...),
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
		return scrollBody(lines, width, height, scroll)
	}
	bodyWidth := max(1, width-2)
	lines := helpReading(bodyWidth)
	for _, section := range []helpSection{topHelp, tuningHelp, wordsHelp, loadHelp} {
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
		prefix := fmt.Sprintf("► %02d  %3d  %-8s ", example.core, example.offset, example.state)
		if example.state == "PARKED" {
			prefix = "  " + prefix[4:]
		}
		gauge := example.gauge + strings.Repeat(" ", max(0, 50-ansi.StringWidth(example.gauge)))
		out = append(out, ansi.Truncate(white.Render(prefix)+gauge+"  "+red.Render(fmt.Sprintf("%3d", example.failure)), width, ""))
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

func helpSectionLines(section helpSection, width int, spaced bool) []string {
	out := []string{helpRule(section.title, width), ""}
	labelWidth := 0
	for _, item := range section.items {
		labelWidth = max(labelWidth, ansi.StringWidth(item.label))
	}
	labelWidth = min(labelWidth+2, max(1, width/3))
	for _, item := range section.items {
		text := wrapStyled(item.text, max(1, width-labelWidth), textStyle)
		for row, line := range text {
			label := strings.Repeat(" ", labelWidth)
			if row == 0 {
				name := ansi.Truncate(item.label, labelWidth-1, "")
				label = white.Render(name) + strings.Repeat(" ", labelWidth-ansi.StringWidth(name))
			}
			out = append(out, ansi.Truncate(label+line, width, ""))
		}
		if spaced {
			out = append(out, "")
		}
	}
	return append(out, "", "")
}

func helpRule(title string, width int) string {
	return white.Render(title) + " " + grey.Render(strings.Repeat("─", max(0, width-ansi.StringWidth(title)-1)))
}

func renderLogBody(s Snapshot, width, height, scroll int) ([]string, int) {
	bodyWidth := max(1, width-2)
	lines := []string{helpRule("JOURNAL", bodyWidth), ""}
	if len(s.log) == 0 {
		lines = append(lines, grey.Render("No journal entries yet."))
	}
	for _, e := range s.log {
		stamp := grey.Render(e.at.Format("15:04:05")) + "  "
		tag := trimWords(vtText(e.tag), 20)
		prefix := stamp + toneStyle(e.tone).Render(fmt.Sprintf("%-20s", tag)) + "  "
		room := max(0, bodyWidth-ansi.StringWidth(prefix))
		text := e.text
		if ansi.StringWidth(text) > room {
			text = trimWords(text, max(0, room-3)) + "..."
		}
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
