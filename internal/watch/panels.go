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

// tableRow places each styled value in its column. A value runs on through the columns after it while they are
// empty, so a row that leaves a column blank can use its cells; a column of width 0 is skipped.
func tableRow(cols []column, values ...string) string {
	var b strings.Builder
	x := 0
	for i, col := range cols {
		if i >= len(values) || col.w <= 0 || values[i] == "" || col.at < x {
			continue
		}
		width := col.w
		for j := i + 1; j < len(cols); j++ {
			if cols[j].w <= 0 {
				continue
			}
			if j < len(values) && values[j] != "" {
				break
			}
			width = cols[j].at + cols[j].w - col.at
		}
		value := trimWords(values[i], width)
		b.WriteString(strings.Repeat(" ", col.at-x))
		b.WriteString(value)
		x = col.at + ansi.StringWidth(value)
	}
	return b.String()
}

// fieldRow is a label/value row: the label in grey, the value starting at the plan's value column.
func fieldRow(t tables, label, value string, width int) string {
	if label == "" {
		return trimWords(strings.Repeat(" ", t.field)+value, width)
	}
	return trimWords(grey.Render(fmt.Sprintf("%-*s", t.field, label))+value, width)
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

func (s Snapshot) contextLines(p layout, now time.Time) []string {
	w, t := p.context.w, p.tables
	if s.hunt != nil {
		return s.huntLines(t, w, p.class, now)
	}
	if len(s.turns) > 0 {
		return s.turnLines(t, w, p.class)
	}
	r7 := s.r7Lines(p)
	room := p.context.h
	if len(r7) > 0 {
		room -= len(r7) + 1
	}
	// lines also returns the running part's row, or -1.
	lines := func(height int) ([]string, int) {
		if s.phase == journal.PhaseDeepening && s.deepen != nil {
			return s.deepenLines(t, w), -1
		}
		var out []string
		focus := -1
		if s.cycle != nil {
			out, focus = s.cycleLines(t, w, p.class, height)
		}
		if len(s.combos) > 0 {
			if len(out) > 0 {
				out = append(out, "")
			}
			out = append(out, s.combinationLines(t, w, p.class)...)
		}
		return out, focus
	}
	out, _ := lines(room)
	if len(r7) == 0 {
		return out
	}
	if len(out) > room {
		// The R7 lines take the panel's last rows, so the rows above end with what they leave out, unless that count
		// would push out the running part.
		if bounded, focus := lines(room - 1); focus < room-1 {
			out = boundedRows(bounded, room)
		} else {
			out = out[:max(room, 0)]
		}
	}
	if len(out) > 0 {
		out = append(out, "")
	}
	return append(out, r7...)
}

// cycleLines keeps the running part, or else the current step, within height rows: it leaves out the rows above it
// from the top, then the blank under the rule. It also returns that row's index, or -1 when no step is open.
func (s Snapshot) cycleLines(t tables, width int, class sizeClass, height int) ([]string, int) {
	g := s.cycle
	done := 0
	for _, step := range g.steps {
		if step.done {
			done++
		}
	}
	out := []string{rule(width, grey.Render(fmt.Sprintf("CYCLE %d", g.number)), grey.Render(fmt.Sprintf("%d steps · %d done", len(g.steps), done))), ""}
	keep := []int{0}
	focus := -1
	scheduleShown := t.cycle[5].w > 0
	if class != compactLayout {
		keep = append(keep, len(out))
		out = append(out, tableRow(t.cycle, "", grey.Render("#"), grey.Render("kind"), grey.Render("where"), grey.Render("workload"), grey.Render("schedule"), grey.Render("result")))
	}
	for i, step := range g.steps {
		marker, style, code := stageMarker("○"), grey, textStyle
		result := ""
		switch {
		case step.done:
			marker, style, code = stageMarker("■"), textStyle, white
			result = green.Render("done")
			if len(step.hunts) > 0 {
				word := "hunt "
				if len(step.hunts) > 1 {
					word = "hunts "
				}
				result += grey.Render(" · " + word + numberRanges(step.hunts))
			}
		case i == g.current:
			marker, style, code = stageMarker("►"), white, white
			result = lit.Render(s.stepPosition(step))
			if g.paused || s.hunt != nil {
				marker, result = amber.Render("○"), amber.Render("paused")
				if part := firstOpenPart(step); len(step.parts) > 1 && part > 0 {
					result = amber.Render(fmt.Sprintf("paused at part %d", part))
				}
			}
		}
		where, schedule := stepSchedule(step)
		if !scheduleShown {
			where = schedule
		}
		kind := code.Render(string(step.regime)) + style.Render(" "+kindWords(step.regime))
		if i == g.current && !step.done {
			keep, focus = append(keep, len(out)), len(out)
		}
		out = append(out, tableRow(t.cycle, marker, style.Render(fmt.Sprintf("%2d", i+1)), kind, style.Render(where), grey.Render(workloadDisplay(step.workload)), style.Render(schedule), result))
		if i == g.current && !step.done {
			for j, line := range s.cyclePartLines(t, step) {
				if j < len(step.parts) && step.parts[j].running {
					focus = len(out)
				}
				out = append(out, line)
			}
		}
	}
	excess := focus + 1 - height
	if excess <= 0 {
		return out, focus
	}
	drop := map[int]bool{}
	for i := 2; i < focus && len(drop) < excess; i++ {
		if !slices.Contains(keep, i) {
			drop[i] = true
		}
	}
	if len(drop) < excess {
		drop[1] = true
	}
	fitted := make([]string, 0, len(out)-len(drop))
	for i, line := range out {
		if !drop[i] {
			fitted = append(fitted, line)
		}
	}
	// Every dropped row is above the running part.
	return fitted, focus - len(drop)
}

// stepPosition is where the running step is: its part, or its trial when it has one part.
func (s Snapshot) stepPosition(step cycleStep) string {
	if len(step.parts) > 1 {
		if part := firstOpenPart(step); part > 0 {
			return fmt.Sprintf("part %d of %d", part, len(step.parts))
		}
	}
	if t := s.trial; t != nil && t.of > 0 {
		return fmt.Sprintf("trial %d of %d", t.index, t.of)
	}
	return ""
}

// firstOpenPart counts from 1: the running part, or else the first one not done.
func firstOpenPart(step cycleStep) int {
	for i, part := range step.parts {
		if part.running {
			return i + 1
		}
	}
	for i, part := range step.parts {
		if !part.done {
			return i + 1
		}
	}
	return 0
}

func (s Snapshot) cyclePartLines(t tables, step cycleStep) []string {
	var out []string
	if len(step.parts) == 1 && !step.more {
		return nil
	}
	partials := false
	for j, part := range step.parts {
		marker, style, faint := "", textStyle, grey
		if part.running {
			marker, style, faint = stageMarker("►"), white, white
		}
		where := coreIDs(part.cores)
		tree := "├"
		if j == len(step.parts)-1 && !step.more {
			tree = "└"
		}
		partials = partials || !part.full
		out = append(out, tableRow(t.cycle, marker, "", track.Render(tree)+" "+style.Render(partName(step.regime, part)), style.Render(where), "", faint.Render(partSchedule(part)), s.partResult(part)))
	}
	if step.more {
		pending := "partials may follow"
		if partials {
			pending = "more partials may follow"
		}
		out = append(out, tableRow(t.cycle, "", "", track.Render("└")+" "+grey.Render(pending)))
	}
	return out
}

func partName(regime machine.Regime, part cyclePart) string {
	if regime != machine.R7 {
		return "core " + coreIDs(part.cores)
	}
	label := "partial"
	if part.full {
		label = "full"
	}
	if part.ccd < 0 {
		return label + " CCD 0+1"
	}
	return fmt.Sprintf("%s CCD %d", label, part.ccd)
}

func (s Snapshot) partResult(part cyclePart) string {
	switch {
	case part.running && s.trial != nil && s.trial.of > 0:
		return lit.Render(fmt.Sprintf("trial %d of %d", s.trial.index, s.trial.of))
	case part.passed+part.failed == 0:
		return ""
	case part.failed > 0:
		return red.Render(fmt.Sprintf("%d failed", part.failed))
	case part.done:
		return green.Render(fmt.Sprintf("%d/%d passed", part.passed, part.short+part.long))
	}
	return textStyle.Render(fmt.Sprintf("%d/%d passed", part.passed, part.short+part.long))
}

func partSchedule(p cyclePart) string {
	var out []string
	switch {
	case p.short == 1:
		out = append(out, shortDuration(p.shortLen))
	case p.short > 1:
		out = append(out, fmt.Sprintf("%d x %s", p.short, shortDuration(p.shortLen)))
	}
	switch {
	case p.long == 1:
		out = append(out, shortDuration(p.longLen))
	case p.long > 1:
		out = append(out, fmt.Sprintf("%d x %s", p.long, shortDuration(p.longLen)))
	}
	return strings.Join(out, " + ")
}

func stepSchedule(step cycleStep) (string, string) {
	if step.regime == machine.R7 {
		parts := fmt.Sprint(len(step.parts))
		if step.more {
			parts += "+"
		}
		return fmt.Sprintf("%d CCDs · %s parts", countCCDs(step.parts), parts), parts + " parts"
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

func (s Snapshot) combinationLines(t tables, width int, class sizeClass) []string {
	right := "offsets at which these cores failed together"
	if class != wideLayout {
		right = "failed together"
	}
	out := []string{rule(width, grey.Render("COMBINATIONS"), grey.Render(right)), ""}
	var ids []int
	for _, combo := range s.combos {
		for _, member := range combo.members {
			if !slices.Contains(ids, member.Core) {
				ids = append(ids, member.Core)
			}
		}
	}
	slices.Sort(ids)
	shown := 0
	if t.member > 0 {
		shown = min(len(ids), t.combos[1].w/t.member)
	}
	offsets := func(value func(id int) (string, lipgloss.Style)) string {
		var b strings.Builder
		for i, id := range ids[:shown] {
			text, style := value(id)
			if i > 0 {
				b.WriteString(strings.Repeat(" ", t.member-3))
			}
			b.WriteString(style.Render(fmt.Sprintf("%3s", text)))
		}
		return b.String()
	}
	found := "found by"
	if t.combos[2].w < len(found) {
		found = "hunt"
	}
	if class != compactLayout {
		out = append(out, tableRow(t.combos, "", offsets(func(id int) (string, lipgloss.Style) { return fmt.Sprintf("%02d", id), grey }), grey.Render(found), grey.Render("against the profile now")))
	}
	if 0 < shown && shown < len(ids) {
		// Too many member cores for the columns: say which ones the rows leave out rather than hide them.
		out = append(out, grey.Render(trimWords(fmt.Sprintf("columns show %d of %d member cores; not shown: %s · l: last %d events", shown, len(ids), coreIDs(ids[shown:]), logLimit), width)))
	}
	for _, combo := range s.combos {
		member := func(id int) (string, lipgloss.Style) {
			for _, m := range combo.members {
				if m.Core != id {
					continue
				}
				switch {
				case combo.holds != nil && combo.holds.Core == id:
					return fmt.Sprint(m.Offset), magenta
				case slices.Contains(combo.clear, id):
					return fmt.Sprint(m.Offset), textStyle
				}
				return fmt.Sprint(m.Offset), grey
			}
			return "", grey
		}
		how := "unresolved"
		if combo.probed {
			how = "probed"
		}
		by := textStyle.Render(fmt.Sprintf("hunt %d", combo.hunt))
		if t.combos[2].w >= 18 {
			by += grey.Render(", " + how)
		}
		against := ""
		switch {
		case combo.holds != nil:
			against = magenta.Render(fmt.Sprintf("holds core %02d at %d", combo.holds.Core, combo.holds.Offset))
		case len(combo.clear) > 0:
			against = grey.Render("clear: ") + textStyle.Render(coreIDs(combo.clear)+" shallower")
		}
		out = append(out, tableRow(t.combos, magenta.Render(fmt.Sprintf("C%d", combo.id)), offsets(member), by, against))
	}
	if shown > 0 {
		now := offsets(func(id int) (string, lipgloss.Style) {
			if c := s.core(id); c != nil {
				return fmt.Sprint(c.profile), white
			}
			return "", white
		})
		out = append(out, "", tableRow(t.combos, white.Render("now"), now))
	}
	return out
}

func (s Snapshot) huntLines(t tables, width int, class sizeClass, now time.Time) []string {
	h := s.hunt
	right := "started " + wallMinute(h.started) + " · " + age(now.Sub(h.started)) + " so far"
	if class == compactLayout {
		right = wallMinute(h.started) + " · " + age(now.Sub(h.started))
	}
	out := []string{rule(width, grey.Render(fmt.Sprintf("HUNT %d", h.id)), grey.Render(right)), ""}
	out = append(out, fieldRow(t, "cause", textStyle.Render(s.huntCauseWords(class)), width))
	if class != compactLayout {
		if evidence := crashEvidence(h.cause.end); evidence != "" {
			out = append(out, fieldRow(t, "evidence", grey.Render(evidence+" · not enough to attribute"), width))
		}
		out = append(out, fieldRow(t, "candidates", textStyle.Render(coreIDs(h.candidates)+", every core with an offset in the failing profile"), width))
	}
	switch {
	case len(h.probes) > 0:
		out = append(out, "")
		out = append(out, s.huntGroupLines(t, width)...)
		out = append(out, "")
		out = append(out, s.probeLines(t, class)...)
	case len(h.plan) > 0:
		if class != compactLayout {
			out = append(out, "")
		}
		out = append(out, s.huntPartLines(t, width, class)...)
	case len(h.groups) > 0:
		out = append(out, "")
		out = append(out, s.huntGroupLines(t, width)...)
	}
	out = append(out, "")
	return append(out, s.huntThen(t, width, class)...)
}

func (s Snapshot) huntCauseWords(class sizeClass) string {
	h := s.hunt
	if h.cause.known {
		return h.cause.knownWords()
	}
	if h.cause.trial.regime == "" {
		return "crash while idle, with no trial running"
	}
	text := signalText(h.cause.signal) + " in "
	if h.cause.rerunOf {
		text += "the rerun of "
	}
	text += trialName(h.cause.trial)
	if class == compactLayout {
		if h.cause.core == nil {
			text += ", no core named"
		}
		return text
	}
	if where := cyclePlace(h.cause.trial); where != "" {
		text += " · " + where
	}
	if h.cause.core == nil {
		text += " · no core named"
	}
	return text
}

func (s Snapshot) huntPartLines(t tables, width int, class sizeClass) []string {
	h := s.hunt
	var out []string
	parked := "parked"
	if h.parkedZero && t.parts[3].w >= len("parked at 0") {
		parked = "parked at 0"
	}
	failing := "at failing offsets"
	if t.parts[2].w < len(failing) {
		failing = "failing"
	}
	if class != compactLayout {
		out = append(out, overlay(grey.Render("parts"), tableRow(t.parts, "", grey.Render("part"), grey.Render(failing), grey.Render(parked), grey.Render("schedule"), grey.Render("state"))))
	}
	for i, part := range h.plan {
		marker, style, faint := "", textStyle, grey
		state := grey.Render(groupOutcomeWords(part.outcome, ""))
		switch {
		case part.running:
			marker, style, faint = stageMarker("►"), white, textStyle
			if s.trial != nil && s.trial.of > 0 {
				state = lit.Render(fmt.Sprintf("trial %d of %d", s.trial.index, s.trial.of))
				if class == compactLayout {
					state = lit.Render(fmt.Sprintf("trial %d/%d", s.trial.index, s.trial.of))
				}
			}
		case part.group == 0 && i > 0:
			state = grey.Render(fmt.Sprintf("if part %d passes", i))
		case part.group == 0:
			state = grey.Render("next")
		case part.outcome == "pass":
			state = green.Render("passed")
		case part.outcome == "failure" || part.outcome == "fail":
			state = red.Render("failed")
		}
		name := fmt.Sprintf("%d of %d", i+1, len(h.plan))
		cores := coreIDs(part.failing)
		if class == compactLayout {
			name = fmt.Sprintf("part %d/%d", i+1, len(h.plan))
			cores += " at failing offsets"
		}
		out = append(out, tableRow(t.parts, marker, style.Render(name), style.Render(cores), textStyle.Render(coreIDs(part.parked)), faint.Render(fmt.Sprintf("%d x %s", part.trials, shortDuration(part.length))), state))
	}
	if class != compactLayout && len(h.plan) > 1 {
		out = append(out, fieldRow(t, "", grey.Render("then finer parts and their complements, until one fails or none does"), width))
	}
	return out
}

// overlay writes label over the start of a row that leaves those cells blank; a row without that gutter keeps its text.
func overlay(label, row string) string {
	n := ansi.StringWidth(label)
	if ansi.StringWidth(row) <= n {
		return label
	}
	if strings.TrimSpace(ansi.Strip(ansi.Truncate(row, n+1, ""))) != "" {
		return row
	}
	return label + ansi.TruncateLeft(row, n, "")
}

// groupRun is a stretch of consecutive groups that ended the same way.
type groupRun struct {
	first, last groupView
	cores       []int
	stages      []string
	passes      int
}

func (s Snapshot) huntGroupLines(t tables, width int) []string {
	h := s.hunt
	var runs []groupRun
	for _, g := range h.groups {
		if g.probe != nil {
			continue
		}
		if n := len(runs); n > 0 && g.stage != "locate" && runs[n-1].last.stage != "locate" && runs[n-1].last.outcome == g.outcome && runs[n-1].last.inferred == g.inferred && g.outcome != "running" && g.outcome != "failure" {
			r := &runs[n-1]
			r.last = g
			r.passes += g.passes
			for _, c := range g.cores {
				if !slices.Contains(r.cores, c) {
					r.cores = append(r.cores, c)
				}
			}
			if !slices.Contains(r.stages, g.stage) {
				r.stages = append(r.stages, g.stage)
			}
			continue
		}
		runs = append(runs, groupRun{first: g, last: g, cores: slices.Clone(g.cores), stages: []string{g.stage}, passes: g.passes})
	}
	var out []string
	for i, r := range runs {
		label := ""
		if i == 0 {
			label = "parts"
		}
		which := fmt.Sprintf("group %d", r.first.id)
		what := coreIDs(r.first.cores) + " at failing offsets"
		if rest := without(h.candidates, r.first.cores); len(rest) > 0 && ansi.StringWidth(what+", "+coreIDs(rest)+" parked") <= t.groups[1].w {
			what += ", " + coreIDs(rest) + " parked"
		}
		if r.first.stage == "locate" {
			what = "locate: " + coreIDs(h.candidates) + " at 0"
		}
		if r.first.id != r.last.id {
			which = fmt.Sprintf("groups %d-%d", r.first.id, r.last.id)
			what = "parts of " + coreIDs(r.cores)
			if slices.Contains(r.stages, "complement") {
				what += " and their complements"
			}
		}
		outcome := groupOutcomeWords(r.last.outcome, r.last.signal)
		style := textStyle
		switch r.last.outcome {
		case "pass":
			style = green
			if r.first.id != r.last.id {
				outcome = "all passed"
			}
		case "failure", "fail":
			style = red
		case "running":
			style = lit
			outcome = fmt.Sprintf("running · %d/%d passed", r.last.passes, r.last.needed)
		}
		result := style.Render(outcome)
		if r.last.inferred {
			result += grey.Render(" · answered by existing evidence")
		}
		out = append(out, overlay(grey.Render(label), tableRow(t.groups, textStyle.Render(which), grey.Render(what), result)))
	}
	if len(h.probes) > 0 {
		members := make([]int, len(h.probes))
		for i, probe := range h.probes {
			members[i] = probe.member
		}
		out = append(out, fieldRow(t, "", white.Render("→ "+coreIDs(members)+" kept together; probing members"), width))
	}
	return out
}

func groupOutcomeWords(outcome string, signal machine.Signal) string {
	switch outcome {
	case "pass":
		return "passed"
	case "failure", "fail":
		if signal == machine.Crash {
			return "crashed"
		}
		return "failed"
	case "running":
		return "running"
	case "skipped":
		return "skipped"
	}
	return vtText(outcome)
}

func without(ids, drop []int) []int {
	var out []int
	for _, id := range ids {
		if !slices.Contains(drop, id) {
			out = append(out, id)
		}
	}
	return out
}

func (s Snapshot) probeLines(t tables, class sizeClass) []string {
	h := s.hunt
	var out []string
	if class != compactLayout {
		out = append(out, overlay(grey.Render("probes"), tableRow(t.probes, grey.Render("member"), grey.Render("now"), grey.Render("group failed at"), grey.Render("passed at"), grey.Render("state"))))
	}
	next := true
	var waiting []int
	flush := func() {
		if len(waiting) > 0 {
			out = append(out, tableRow(t.probes, textStyle.Render(coreIDs(waiting)), "", "", "", grey.Render("waiting")))
			waiting = nil
		}
	}
	for _, probe := range h.probes {
		probed := probe.running || probe.done || len(probe.passedAt) > 0 || len(probe.failedAt) > 1
		if !probed && !next {
			waiting = append(waiting, probe.member)
			continue
		}
		flush()
		style, state := textStyle, grey.Render("next")
		switch {
		case probe.done:
			state = green.Render("done")
		case probe.running && s.trial != nil && s.trial.of > 0:
			style, state = white, lit.Render(fmt.Sprintf("► trial %d of %d", s.trial.index, s.trial.of))
			next = false
		case probe.running:
			style, state = white, lit.Render("► running")
			next = false
		default:
			next = false
		}
		failed, passed := "", ""
		if probed {
			// The first entry is the failing offset the ladder starts from; a short hunt profile may lack it.
			if len(probe.failedAt) > 1 {
				failed = red.Render(offsetList(probe.failedAt[1:]))
			}
			passed = green.Render(s.passedWords(probe))
		}
		out = append(out, tableRow(t.probes, style.Render(fmt.Sprintf("%02d", probe.member)), style.Render(fmt.Sprint(probe.now)), failed, passed, state))
	}
	flush()
	return out
}

// passedWords lists the offsets at which the group passed with this member there, each with how it passed.
func (s Snapshot) passedWords(probe probeView) string {
	parts := make([]string, 0, len(probe.passedAt))
	for _, offset := range probe.passedAt {
		if slices.Contains(probe.carried, offset) {
			parts = append(parts, fmt.Sprintf("%d carried", offset))
			continue
		}
		text := fmt.Sprint(offset)
		for _, g := range s.hunt.groups {
			if g.probe != nil && g.probe.Core == probe.member && g.probe.Offset == offset {
				text = fmt.Sprintf("%d %d/%d", offset, g.passes, g.needed)
			}
		}
		parts = append(parts, text)
	}
	return strings.Join(parts, ", ")
}

func (s Snapshot) huntThen(t tables, width int, class sizeClass) []string {
	h := s.hunt
	steps := []string{"record what it found: one core's failure point, or a combination", "back off past it"}
	if len(h.probes) > 0 {
		members := make([]int, len(h.probes))
		for i, probe := range h.probes {
			members[i] = probe.member
		}
		steps = []string{"record the combination: " + coreIDs(members) + " at the offsets that still fail", "back off one member to a tested passing probe that keeps the profile clear"}
	}
	if class == compactLayout {
		steps = []string{"record a failure point or a combination", "back off past it"}
	}
	for i, step := range steps {
		steps[i] = textStyle.Render(step)
	}
	if r := h.rerun; r.short > 0 || r.long > 0 {
		where, length := rerunWords(r, class)
		steps = append(steps, textStyle.Render("rerun ")+white.Render(where)+textStyle.Render(length))
	}
	if g := s.cycle; g != nil && g.current < len(g.steps) && class != compactLayout {
		// Where the cycle resumes depends on what the rerun's passes complete, which the tuner does not project
		// this far ahead; name only the cycle.
		steps = append(steps, textStyle.Render("resume ")+white.Render(fmt.Sprintf("cycle %d", g.number)))
	}
	var out []string
	if class == compactLayout {
		out = append(out, overlay(grey.Render("then"), tableRow(t.steps, grey.Render("1"), steps[0])))
	} else {
		out = append(out, fieldRow(t, "then", grey.Render("if the hunt resolves and the rerun passes:"), width))
		out = append(out, tableRow(t.steps, grey.Render("1"), steps[0]))
	}
	for i, step := range steps[1:] {
		out = append(out, tableRow(t.steps, grey.Render(fmt.Sprint(i+2)), step))
	}
	return out
}

// rerunWords says where the rerun after a hunt loads and how long it runs.
func rerunWords(r rerunPlan, class sizeClass) (string, string) {
	where := string(r.regime) + " " + kindWords(r.regime)
	if class == compactLayout {
		length := fmt.Sprintf(": %d x %s", r.short, shortDuration(r.shortLen))
		if r.long > 0 {
			length += fmt.Sprintf(", then %d x %s", r.long, shortDuration(r.longLen))
		}
		return where, length
	}
	length := fmt.Sprintf(": %s of %s", plural(r.short, "trial"), shortDuration(r.shortLen))
	if r.long > 0 {
		length += fmt.Sprintf(", then %d of %s", r.long, shortDuration(r.longLen))
	} else {
		length += ", its original length"
	}
	return where + " on " + coreIDs(r.cores), length
}

func plural(n int, word string) string {
	switch {
	case n == 1:
		return "1 " + word
	case strings.HasSuffix(word, "sh"):
		return fmt.Sprintf("%d %ses", n, word)
	}
	return fmt.Sprintf("%d %ss", n, word)
}

func (s Snapshot) turnLines(t tables, width int, class sizeClass) []string {
	out := []string{rule(width, grey.Render("TURNS"), grey.Render("one core per turn, in this order")), ""}
	if class != compactLayout {
		out = append(out, tableRow(t.turns, "", grey.Render("core"), grey.Render("this turn"), grey.Render("at"), grey.Render("workload"), grey.Render("so far")))
	}
	for _, turn := range s.turns {
		marker, style, faint := "", textStyle, grey
		if turn.running {
			marker, style, faint = stageMarker("►"), white, textStyle
		}
		what := "search step: " + regimeList(turn.regimes)
		if turn.step == 1 {
			what = "search step: 1 count deeper"
		}
		sofar := "none yet"
		if core := s.core(turn.core); core != nil {
			if turn.confirm && core.confirm != nil {
				n := core.confirm
				what = fmt.Sprintf("confirm: light trial %d of %d", min(n.light+1, n.needed), n.needed)
				if n.light >= n.needed {
					what = fmt.Sprintf("confirm: heavy trial %d of %d", min(n.heavy+1, n.needed), n.needed)
				}
				sofar = fmt.Sprintf("light %d/%d, heavy %d/%d", n.light, n.needed, n.heavy, n.needed)
			} else if note := ansi.Strip(core.noteLine(34)); note != "" {
				sofar = note
			}
		}
		out = append(out, tableRow(t.turns, marker, style.Render(fmt.Sprintf("%02d", turn.core)), style.Render(what), style.Render(fmt.Sprint(turn.offset)), grey.Render(workloadLabel(turn.workload.ID)), faint.Render(sofar)))
	}
	var found []int
	for _, core := range s.cores {
		if core.solo != nil {
			found = append(found, core.id)
		}
	}
	if len(found) > 0 {
		out = append(out, "", trimWords("  "+grey.Render("waiting for the cycles  ")+textStyle.Render(coreIDs(found))+grey.Render("  · solo limits found"), width))
	}
	return out
}

func (s Snapshot) deepenLines(t tables, width int) []string {
	d := s.deepen
	out := []string{rule(width, grey.Render(fmt.Sprintf("DEEPEN · ROUND %d", d.round)), ""), ""}
	if d.waiting {
		out = append(out, textStyle.Render("Waiting for a passed full cycle."))
	}
	out = append(out, fieldRow(t, "has room", textStyle.Render(coreIDs(d.room)+" · in this order"), width))
	for i, target := range d.profile {
		if i >= len(s.cores) || target == s.cores[i].profile {
			continue
		}
		verb := "goes deeper"
		if target > s.cores[i].profile {
			verb = "yields"
		}
		out = append(out, fieldRow(t, "move", textStyle.Render(fmt.Sprintf("core %02d %s: %d → %d", s.cores[i].id, verb, s.cores[i].profile, target)), width))
	}
	out = append(out, "", grey.Render("CHECKS"))
	for _, check := range d.checks {
		out = append(out, trimWords(textStyle.Render(fmt.Sprintf("%s on %s  %d/%d passed", check.Regime, coreIDs(check.Cores), check.Passes, check.Needed))+"  "+grey.Render(workloadLabel(check.Workload)), width))
	}
	return out
}

// numberRanges writes numbers compactly: a run of consecutive numbers becomes a range.
func numberRanges(values []int) string {
	sorted := slices.Sorted(slices.Values(values))
	var parts []string
	for i := 0; i < len(sorted); {
		j := i
		for j+1 < len(sorted) && sorted[j+1] == sorted[j]+1 {
			j++
		}
		if j > i {
			parts = append(parts, fmt.Sprintf("%d-%d", sorted[i], sorted[j]))
		} else {
			parts = append(parts, fmt.Sprint(sorted[i]))
		}
		i = j + 1
	}
	return strings.Join(parts, ", ")
}

// huntStage says where the hunt is: its parts, its member probes, or starting before its first group.
func (s Snapshot) huntStage(short bool) string {
	h := s.hunt
	switch {
	case len(h.probes) > 0:
		at := len(h.probes)
		for i, probe := range h.probes {
			if !probe.done {
				at = i + 1
				break
			}
		}
		if short {
			return fmt.Sprintf("member %d of %d", at, len(h.probes))
		}
		return fmt.Sprintf("member probes · member %d of %d", at, len(h.probes))
	case len(h.groups) == 0:
		return "starting"
	case h.locating():
		return "locate"
	case len(h.plan) > 0:
		// The part in flight, else one started but still short of its passes, else the next unstarted one.
		at := 1 + slices.IndexFunc(h.plan, func(p huntPart) bool { return p.running })
		if at == 0 {
			at = 1 + slices.IndexFunc(h.plan, func(p huntPart) bool { return p.group != 0 && p.outcome == "running" })
		}
		if at == 0 {
			at = 1 + slices.IndexFunc(h.plan, func(p huntPart) bool { return p.group == 0 })
		}
		if at == 0 {
			return fmt.Sprintf("group %d", h.groups[len(h.groups)-1].id)
		}
		if short {
			return fmt.Sprintf("part %d of %d", at, len(h.plan))
		}
		return fmt.Sprintf("parts · part %d of %d", at, len(h.plan))
	}
	return fmt.Sprintf("group %d", h.groups[len(h.groups)-1].id)
}
