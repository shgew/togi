package watch

import (
	"fmt"
	"slices"
	"strings"
	"time"

	"charm.land/lipgloss/v2"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

// story is what the narrator says: why the screen looks the way it does, never what the NOW band or the outcome
// lines already say.
type story struct {
	label string   // the stage it speaks for
	lines []string // at most two on the dashboard
	brief string   // the one line a compact screen has room for
	tone  tone
}

func (s Snapshot) story(now time.Time) story {
	switch {
	case s.problem != nil:
		return story{"I CAN'T READ THE JOURNAL", []string{vtText(s.problem.Error()), "I'll retry when the journal changes. Tuning itself is not affected by this screen."}, "", badTone}
	case !s.session:
		return story{"NO SESSION YET", []string{"Nothing has been recorded yet. Start with togi run, or boot the togi entry, and I'll tell you what I'm doing here."}, "", plainTone}
	case s.deadEnd != nil:
		return s.deadEndStory()
	case s.stopped != nil:
		text := "I've stopped. Everything I learned is in the journal; togi run picks up where I left off."
		if s.stopped.saved {
			text = "I restored safer offsets before stopping. togi run picks up where I left off."
		}
		return story{"STOPPED", []string{text}, text, plainTone}
	case s.recover != nil:
		return s.recoverStory()
	case s.trial == nil:
		text := "The last trial has ended; the tuner records what it decided before the next one starts."
		return story{s.stageLabel(), []string{text}, text, plainTone}
	}
	t := s.trial
	if !t.hasStarted {
		text := "The trial's intent is recorded and its offsets are set; its workload is starting."
		return story{s.stageLabel(), []string{text}, text, plainTone}
	}
	if s.hunt != nil {
		return s.huntStory()
	}
	if t.condition == machine.Alone {
		c := s.core(t.core)
		text := fmt.Sprintf("I'm finding core %02d's solo limit, one turn at a time, now at %d.", t.core, t.offset)
		if c != nil && c.confirm != nil {
			text = fmt.Sprintf("%d is core %02d's candidate solo limit, and I'm confirming it.", c.confirm.offset, c.id)
			if c.fail != nil {
				text = fmt.Sprintf("Core %02d failed at %d, so %d is its candidate solo limit, and I'm confirming it.", c.id, *c.fail, c.confirm.offset)
			}
		}
		return story{"SOLO LIMITS", []string{text, "One core at a time with the rest at 0, so any failure belongs to the loaded core."}, text, plainTone}
	}
	if t.rerun {
		lines := []string{"The failed load runs again after the offsets changed. This trial checks the new profile."}
		if g := s.cycle; g != nil && g.current < len(g.steps) {
			lines = append(lines, fmt.Sprintf("Cycle %d waits at step %d until the rerun passes.", g.number, g.current+1))
		}
		brief := "The failed load runs again on the new profile."
		if len(lines) > 1 {
			brief = "The failed load runs again on the new profile; " + strings.ToLower(lines[1][:1]) + lines[1][1:]
		}
		return story{"RERUN", lines, brief, plainTone}
	}
	if t.round > 0 || s.phase == journal.PhaseDeepening {
		return story{fmt.Sprintf("DEEPEN · ROUND %d", t.round), []string{"These offsets passed a full cycle. I check the deepening plan's proposed profile together.", "The checks below distinguish cores that go deeper from members that yield shallower."}, "Checking the deepening plan's proposed profile.", plainTone}
	}
	name := fmt.Sprintf("CYCLE %d", t.cycle)
	if t.regime == machine.R6 {
		until := t.started.Add(t.duration)
		if now.Before(until) {
			return story{name, []string{fmt.Sprintf("Step %d leaves every core idle for %s, with short wake-ups, to test idle and boost states.", t.step, minutes(t.duration)), "This screen holds still until " + until.Format("15:04") + ", clock included, so drawing it cannot wake the cores."}, "Every core idle with short wake-ups; this screen holds still.", plainTone}
		}
		text := fmt.Sprintf("The idle trial was planned to end at %s. I'm waiting for its result in the journal.", until.Format("15:04"))
		return story{name, []string{text}, text, plainTone}
	}
	step := fmt.Sprintf("Step %d is %s %s with %s.", t.step, t.regime, kindWords(t.regime), workloadDisplay(t.workload))
	brief := step
	where := "Every core runs at its profile offset. The cycle keeps testing until stopped."
	if t.parts > 1 {
		where = fmt.Sprintf("Part %d of %d loads %s for %s.", t.part, t.parts, coreIDs(t.cores), minutes(t.duration))
		brief = fmt.Sprintf("Step %d, %s %s: part %d of %d loads %s for %s.", t.step, t.regime, kindWords(t.regime), t.part, t.parts, coreIDs(t.cores), minutes(t.duration))
	}
	if t.recordOnly {
		where = fmt.Sprintf("Part %d of %d, on %s, is record only: it covers nothing in the cycle.", t.part, t.parts, coreIDs(t.cores))
		brief = fmt.Sprintf("Step %d, %s %s: part %d of %d, on %s, is record only.", t.step, t.regime, kindWords(t.regime), t.part, t.parts, coreIDs(t.cores))
	}
	if s.cleanCycles > 0 {
		where = fmt.Sprintf("%d clean cycles count for this profile. Passing trials cannot prove it will never fail.", s.cleanCycles)
	}
	return story{name, []string{step, where}, brief, plainTone}
}

// stageLabel names the stage the tuner is in, for a narrator with no trial to speak of.
func (s Snapshot) stageLabel() string {
	switch {
	case s.hunt != nil:
		return fmt.Sprintf("HUNT %d", s.hunt.id)
	case len(s.turns) > 0:
		return "SOLO LIMITS"
	case s.phase == journal.PhaseDeepening:
		return "DEEPEN"
	case s.cycle != nil:
		return fmt.Sprintf("CYCLE %d", s.cycle.number)
	}
	return "TOGI"
}

func (s Snapshot) huntStory() story {
	h, t := s.hunt, s.trial
	label := fmt.Sprintf("HUNT %d", h.id)
	if t.probe != nil {
		var group []int
		for _, g := range h.groups {
			if g.probe == nil {
				group = g.cores
			}
		}
		if len(h.probes) > 0 {
			group = group[:0:0]
			for _, probe := range h.probes {
				group = append(group, probe.member)
			}
		}
		verb := "failed"
		if h.cause.signal == machine.Crash {
			verb = "crashed"
		}
		first := fmt.Sprintf("%s %s together at their offsets, and every smaller part of them that was tested passed.", coreIDs(group), verb)
		which := fmt.Sprintf("Now core %02d.", t.probe.Core)
		if len(h.probes) > 0 && h.probes[0].member == t.probe.Core {
			which = fmt.Sprintf("Core %02d first.", t.probe.Core)
		}
		return story{label, []string{first, "So I probe each member: how shallow must it go for the rest to pass? " + which}, fmt.Sprintf("Probing members of %s: core %02d at %d.", coreIDs(group), t.probe.Core, t.probe.Offset), huntTone}
	}
	cause, named, brief := s.huntCauseStory()
	return story{label, []string{cause, named + "I rerun that load with " + s.splitWords() + "."}, brief + " Rerunning it with " + coreIDs(t.parked) + " parked.", huntTone}
}

// splitWords says how the hunt splits its candidates in the trial in flight.
func (s Snapshot) splitWords() string {
	h := s.hunt
	parked := "parked"
	if h.parkedZero {
		parked = "parked at 0"
	}
	if len(h.plan) == 2 {
		return fmt.Sprintf("half of %s at its offsets, the other half %s", coreIDs(h.candidates), parked)
	}
	return fmt.Sprintf("part of %s at its offsets, the rest %s", coreIDs(h.candidates), parked)
}

// huntCauseStory tells what started the hunt: a sentence, whether a core was named, and a compact line.
func (s Snapshot) huntCauseStory() (string, string, string) {
	c := s.hunt.cause
	named := "No core was named: "
	if c.core != nil {
		named = fmt.Sprintf("Core %02d was named: ", *c.core)
	}
	if c.carried {
		text := trialName(c.trial) + " already failed at these offsets in a carried trial."
		return text, named, text
	}
	what := fmt.Sprintf("the %s %s trial on %s", lengthWords(c.trial.duration), kindWords(c.trial.regime), coreIDs(c.trial.cores))
	if c.rerunOf {
		what = "the rerun of " + what
	}
	if c.signal != machine.Crash {
		return fmt.Sprintf("%s ended with %s.", capitalize(what), signalText(c.signal)), named, fmt.Sprintf("%s in %s; no core named.", capitalize(signalText(c.signal)), trialName(c.trial))
	}
	when := lateness(c.end)
	brief := "Crashed"
	if when != "during" {
		brief += " " + strings.Fields(when)[0]
	}
	return fmt.Sprintf("The machine crashed %s %s and rebooted.", when, what), named, brief + " in " + trialName(c.trial) + "; no core named."
}

func (s Snapshot) recoverStory() story {
	r := s.recover
	if h := s.hunt; h != nil && !h.started.Before(r.bootAt) {
		parked := "parked"
		if h.parkedZero {
			parked = "parked at 0"
		}
		text := fmt.Sprintf("I look for the cause among %s, the cores with an offset in the failing profile.", coreIDs(h.candidates))
		brief := fmt.Sprintf("To find the cause, I rerun that load with part of %s %s.", coreIDs(h.candidates), parked)
		return story{fmt.Sprintf("HUNT %d", h.id), []string{text, "I rerun that load with part of them at their failing offsets and the rest " + parked + "."}, brief, huntTone}
	}
	lines := []string{"I picked up from the journal after the reboot; nothing it recorded is lost."}
	switch {
	case r.trial != nil && r.trial.recordOnly:
		lines = append(lines, "That trial was record only, so the crash moves no offset.")
	case r.end != nil && r.end.core != nil:
		lines = append(lines, fmt.Sprintf("Core %02d was named: its failure point is recorded and it backs off.", *r.end.core))
	}
	return story{"RECOVERED", lines, lines[0], warnTone}
}

func (s Snapshot) deadEndStory() story {
	d := s.deadEnd
	why := map[journal.DeadEndCondition]string{
		journal.DeadEndFailureAtZero: "A core failed even at offset 0, so the problem isn't Curve Optimizer. Check the rest of the system before tuning again.",
		journal.DeadEndSMU:           "The CPU didn't take an offset the way I wrote it, so I can't trust what is applied. Nothing else will be written.",
		journal.DeadEndNoEvidence:    "I couldn't obtain usable failure evidence, so I can't continue testing safely. Check the recorded details before tuning again.",
		journal.DeadEndBootLoop:      "The machine crashed three times in a row before I applied any offsets: something else is crashing it. Check the system before tuning again.",
		journal.DeadEndContainment:   "I couldn't confirm that the stress programs were contained and cleaned up safely, so tuning can't continue. Check the recorded details.",
		journal.DeadEndPreflight:     "A check before tuning failed: this isn't the environment I tune in. Fix the recorded check before tuning again.",
		journal.DeadEndDefect:        "An earlier build may have moved offsets deeper than proven. Answer the reset question at a terminal before tuning continues.",
		journal.DeadEndThermalTrip:   "The CPU shut down from heat. Check the cooling before tuning again.",
	}[d.condition]
	if why == "" {
		why = "I can't make progress from here. Check the recorded details before tuning again."
	}
	return story{"DEAD END", []string{why}, why, badTone}
}

func stopWords(reason journal.ShutdownReason) string {
	switch reason {
	case journal.ShutdownSignal:
		return "stopped by a signal"
	case journal.ShutdownCycles:
		return "requested clean cycles completed"
	case journal.ShutdownCommand:
		return "read-only command finished; no applied offsets changed"
	case journal.ShutdownDeadEnd:
		return "tuning stopped at a dead end"
	}
	return vtText(string(reason))
}

func loadWords(regime machine.Regime) string {
	if regime == machine.R6 {
		return "idle + bursts"
	}
	if words := regimeWords[regime]; words != "" {
		return words
	}
	return vtText(string(regime))
}

// kindWords is the short name of a kind of load, as it follows its regime code.
func kindWords(regime machine.Regime) string {
	switch regime {
	case machine.R1:
		return "light"
	case machine.R2:
		return "heavy vector"
	case machine.R3:
		return "load steps"
	case machine.R4:
		return "partial load"
	case machine.R5:
		return "both threads"
	case machine.R6:
		return "idle + bursts"
	case machine.R7:
		return "all-core"
	}
	return vtText(string(regime))
}

// trialName is a trial's regime, kind and cores, such as "R7 all-core on 00-15".
func trialName(t trialView) string {
	if t.condition == machine.Alone {
		return fmt.Sprintf("core %02d · %s %s at %d", t.core, t.regime, kindWords(t.regime), t.offset)
	}
	return fmt.Sprintf("%s %s on %s", t.regime, kindWords(t.regime), coreIDs(t.cores))
}

func onCores(cores []int) string {
	if len(cores) == 1 {
		return fmt.Sprintf("on core %02d", cores[0])
	}
	return "on " + coreIDs(cores)
}

// cyclePlace is where a checking trial stood in its cycle: "cycle 1 step 2, part 5, trial 4 of 4".
func cyclePlace(t trialView) string {
	if t.cycle == 0 {
		return ""
	}
	text := fmt.Sprintf("cycle %d step %d", t.cycle, t.step)
	if t.parts > 1 && t.part > 0 {
		text += fmt.Sprintf(", part %d", t.part)
	}
	if t.of > 0 {
		text += fmt.Sprintf(", trial %d of %d", t.index, t.of)
	}
	return text
}

// lateness says how far into its planned length a crashed trial got, from its last sample.
func lateness(end *trialEnd) string {
	if end == nil || end.lastSample == nil || end.planned <= 0 {
		return "during"
	}
	switch frac := float64(*end.lastSample) / float64(end.planned); {
	case frac < 1.0/3:
		return "early in"
	case frac < 2.0/3:
		return "midway through"
	}
	return "late in"
}

// crashEvidence is what the journal holds about a crash that falls short of naming a core.
func crashEvidence(end *trialEnd) string {
	if end == nil || end.signal != machine.Crash || end.lastSample == nil {
		return ""
	}
	at := clock(*end.lastSample) + " into the trial"
	if end.stalled != nil {
		return fmt.Sprintf("core %02d's worker had stalled by the last sample, %s", *end.stalled, at)
	}
	return "the last sample came " + at
}

func capitalize(text string) string {
	if text == "" {
		return text
	}
	return strings.ToUpper(text[:1]) + text[1:]
}

// lengthWords is a trial's planned length as an adjective: "10-minute".
func lengthWords(d time.Duration) string {
	if d >= time.Minute && d%time.Minute == 0 {
		return fmt.Sprintf("%d-minute", int(d/time.Minute))
	}
	return fmt.Sprintf("%d-second", int(d/time.Second))
}

func minutes(d time.Duration) string {
	if d >= time.Minute && d%time.Minute == 0 {
		return plural(int(d/time.Minute), "minute")
	}
	return plural(int(d/time.Second), "second")
}

// trialWhere is where the trial in flight loads, with how a hunt sets the cores it does not judge.
func (s Snapshot) trialWhere(t trialView) string {
	if t.condition == machine.Alone {
		return fmt.Sprintf("on core %02d alone at %d", t.core, t.offset)
	}
	where := "on " + coreIDs(t.cores)
	if t.hunt > 0 {
		if shape := s.shapeWords(t.cores, t.profile, s.shapes[t.hunt]); shape != "" {
			return where + " · " + shape
		}
	}
	if t.parts > 0 && t.regime == machine.R7 && len(t.cores) == len(s.cores) && len(s.cores) > 0 {
		return where + ", every CCD"
	}
	return where
}

// shapeWords says which cores a hunt trial runs at their failing offsets and which it parks, and any core it moves
// elsewhere, such as a probed member.
func (s Snapshot) shapeWords(loaded, profile []int, shape huntShape) string {
	var failing, parked, idle []int
	var moved []string
	zero := true
	for i, id := range s.order {
		if i >= len(profile) || i >= len(shape.failing) || i >= len(shape.parked) || shape.failing[i] == shape.parked[i] {
			continue
		}
		switch profile[i] {
		case shape.failing[i]:
			failing = append(failing, id)
		case shape.parked[i]:
			parked = append(parked, id)
			zero = zero && shape.parked[i] == 0
			if !slices.Contains(loaded, id) {
				idle = append(idle, id)
			}
		default:
			moved = append(moved, fmt.Sprintf("core %02d at %d", id, profile[i]))
		}
	}
	if len(failing)+len(parked)+len(moved) == 0 {
		return ""
	}
	var parts []string
	if !slices.Equal(failing, slices.Sorted(slices.Values(loaded))) || len(idle) < len(parked) {
		if len(failing) > 0 {
			parts = append(parts, coreIDs(failing)+" at failing offsets")
		}
	}
	parts = append(parts, moved...)
	if len(parked) > 0 {
		text := coreIDs(parked) + " parked"
		if zero {
			text += " at 0"
		}
		if len(idle) == len(parked) {
			text += ", idle"
		}
		parts = append(parts, text)
	}
	return strings.Join(parts, ", ")
}

func (s Snapshot) operation(t trialView) string {
	text := ""
	tone := lit
	switch {
	case t.hunt > 0:
		tone = amber
		text = fmt.Sprintf("HUNT %d", t.hunt)
		switch {
		case t.probe != nil:
			text += fmt.Sprintf(" · GROUP %d · CORE %02d AT %d", t.group, t.probe.Core, t.probe.Offset)
		case t.huntParts > 0:
			text += fmt.Sprintf(" · PART %d OF %d", t.huntPart, t.huntParts)
		case t.group > 0:
			text += fmt.Sprintf(" · GROUP %d", t.group)
		}
	case t.condition == machine.Alone:
		verb := "SEARCH"
		if c := s.core(t.core); c != nil && c.confirm != nil {
			verb = "CONFIRM"
		}
		text = fmt.Sprintf("%s CORE %02d AT %d", verb, t.core, t.offset)
		switch t.regime {
		case machine.R1:
			text += " · LIGHT"
		case machine.R2:
			text += " · HEAVY"
		case machine.R3, machine.R4, machine.R5, machine.R6, machine.R7:
		}
	case t.rerun:
		text = "RERUN AFTER BACKOFF"
	case t.round > 0:
		text = fmt.Sprintf("DEEPEN · ROUND %d", t.round)
	default:
		text = fmt.Sprintf("CYCLE %d · STEP %d", t.cycle, t.step)
		if t.parts > 1 {
			text += fmt.Sprintf(" · PART %d", t.part)
		}
	}
	if t.recordOnly {
		text += " · RECORD ONLY"
	}
	return tone.Render(text)
}

// endWord is how a trial ended, in one word: a crash is a crash, whatever else the journal says.
func endWord(t trialEnd) (string, lipgloss.Style) {
	switch {
	case t.signal == machine.Crash:
		return "CRASH", red
	case t.outcome == journal.OutcomeFailure:
		return "FAILURE", red
	case t.outcome == journal.OutcomeInconclusive:
		return "INCONCLUSIVE", amber
	}
	return "PASS", green
}

// endFacts are the measurements of a trial that ended: its length, unless a crash cut it short, its cores, its peak
// Tctl and its median voltage request.
func endFacts(t trialEnd) []string {
	where := onCores(t.cores)
	if t.signal != machine.Crash {
		where = clock(t.duration) + " " + where
	}
	facts := []string{where}
	if t.signal != "" && t.signal != machine.Crash {
		facts = append(facts, signalText(t.signal))
	}
	if t.core != nil {
		facts = append(facts, fmt.Sprintf("core %02d named", *t.core))
	}
	if t.tctlMaxC != nil {
		facts = append(facts, fmt.Sprintf("Tctl max %d°C", *t.tctlMaxC))
	}
	if t.voltageV != nil {
		facts = append(facts, fmt.Sprintf("%.2f V median", *t.voltageV))
	}
	return facts
}

func lastDescription(t trialEnd) string {
	word, _ := endWord(t)
	if t.signal != machine.Crash {
		word += " after " + clock(t.duration)
	}
	facts := endFacts(t)
	facts[0] = fmt.Sprintf("%s %s %s", t.regime, kindWords(t.regime), onCores(t.cores))
	return word + " · " + strings.Join(facts, " · ")
}

func (s Snapshot) lastLines() []string {
	if s.last == nil {
		return nil
	}
	word, style := endWord(*s.last)
	return []string{grey.Render("last trial      ") + style.Render(word) + grey.Render("  "+strings.Join(endFacts(*s.last), " · "))}
}

func (s Snapshot) nextLine() string {
	if s.next == nil {
		return "next: deciding"
	}
	return "next: " + s.nextTrialWords(*s.next, nil, huntShape{})
}

// nextTrialWords describes a trial the tuner runs next the way the NOW band describes the trial in flight. A branch
// that starts a hunt passes that hunt's shape.
func (s Snapshot) nextTrialWords(n tuner.Trial, after *trialView, shape huntShape) string {
	what := fmt.Sprintf("%s %s on %s", n.Regime, kindWords(n.Regime), coreIDs(n.Cores))
	if len(n.Cores) == 0 {
		what = fmt.Sprintf("%s %s on core %02d alone at %d", n.Regime, kindWords(n.Regime), n.Core, n.Offset)
	}
	var text string
	switch {
	case n.Hunt > 0:
		if shape.failing == nil {
			shape = s.shapes[n.Hunt]
		}
		layout := s.shapeWords(n.Cores, n.Profile, shape)
		if h := s.hunt; h != nil && h.id == n.Hunt {
			if part := s.planPart(n, shape); part > 0 {
				return fmt.Sprintf("part %d: %s", part, layout)
			}
		}
		text = fmt.Sprintf("hunt %d group %d: %s", n.Hunt, n.Group, what)
		if layout != "" {
			text += " · " + layout
		}
	case n.Rerun:
		text = "rerun " + what + " · " + shortDuration(time.Duration(n.DurationS)*time.Second)
	case n.Cycle > 0:
		if after != nil && after.cycle == n.Cycle && after.step == n.Step {
			if name := s.partWords(n); name != "" {
				return name
			}
		}
		text = fmt.Sprintf("step %d: %s %s", n.Step, n.Regime, kindWords(n.Regime))
		if n.Workload != "" {
			text += " with " + workloadDisplayID(n.Workload)
		}
	case n.Round > 0:
		text = fmt.Sprintf("deepening round %d: %s", n.Round, what)
	default:
		text = what
	}
	if n.RecordOnly {
		text += " · record only"
	}
	return text
}

// planPart finds which part of the running hunt's split a trial loads, counting from 1.
func (s Snapshot) planPart(n tuner.Trial, shape huntShape) int {
	for i, part := range s.hunt.plan {
		failing := true
		for j, id := range s.order {
			if j >= len(n.Profile) || j >= len(shape.failing) || shape.failing[j] == shape.parked[j] {
				continue
			}
			if (n.Profile[j] == shape.failing[j]) != slices.Contains(part.failing, id) {
				failing = false
			}
		}
		if failing {
			return i + 1
		}
	}
	return 0
}

// partWords names the part of the current checking step a trial loads: "part 2: full CCD 0 on 00-07, 5m".
func (s Snapshot) partWords(n tuner.Trial) string {
	g := s.cycle
	if g == nil || n.Step < 1 || n.Step > len(g.steps) {
		return ""
	}
	step := g.steps[n.Step-1]
	want := slices.Sorted(slices.Values(n.Cores))
	for i, part := range step.parts {
		if slices.Equal(slices.Sorted(slices.Values(part.cores)), want) {
			return fmt.Sprintf("part %d: %s on %s, %s", i+1, partName(step.regime, part), coreIDs(n.Cores), shortDuration(time.Duration(n.DurationS)*time.Second))
		}
	}
	return ""
}

type outcomeRow struct {
	label, text, compact string
	style                lipgloss.Style
}

func (s Snapshot) outcomeRows() []outcomeRow {
	order := []premise{ifAllPass, ifPasses, ifFails, ifNamed, ifUnnamed, ifInconclusive}
	if s.hunt != nil {
		order = []premise{ifAllPass, ifPasses, ifUnnamed, ifNamed, ifFails, ifInconclusive}
	}
	var out []outcomeRow
	passShown := false
	for _, kind := range order {
		for _, branch := range s.outcomes {
			if branch.premise != kind || ((kind == ifPasses || kind == ifAllPass) && passShown) {
				continue
			}
			label, style := "if it passes", green
			switch kind {
			case ifPasses:
			case ifAllPass:
				label = fmt.Sprintf("if all %d pass", branch.passes)
			case ifFails:
				label, style = "if it fails", red
			case ifNamed:
				label, style = "if a core is named", red
			case ifUnnamed:
				label, style = "if none is named", red
			case ifInconclusive:
				label, style = "if inconclusive", grey
			}
			out = append(out, outcomeRow{label, outcomeWords(branch), compactOutcome(branch), style})
			if kind == ifAllPass || kind == ifPasses {
				passShown = true
			}
			if len(out) == 3 {
				return out
			}
		}
	}
	return out
}

func compactOutcome(branch outcome) string {
	if branch.needsMCE {
		return "read hardware evidence"
	}
	if branch.needsRanking {
		return "read core ranking"
	}
	if branch.needsHistory {
		return "complete hunt history needed"
	}
	if branch.premise == ifNamed {
		text := ""
		for _, payload := range branch.decisions {
			switch d := payload.(type) {
			case *journal.TunerDecision:
				if d.Decision == journal.Backoff {
					text = "named core backs off"
				}
			case *journal.HuntEnd:
				text = "hunt ends"
			case *journal.DeadEnd:
				return "→ tuning stops"
			}
		}
		if text != "" {
			return "→ " + text
		}
		if branch.next != nil {
			return "→ " + outcomeNextWords(*branch.next, true)
		}
		return "no next trial projected"
	}
	var text string
	for _, decision := range branch.decisions {
		switch d := decision.(type) {
		case *journal.DeadEnd:
			return "stop: " + vtText(d.Detail)
		case *journal.HuntStart:
			text = fmt.Sprintf("hunt %d: %s", d.Hunt, coreIDs(d.Candidates))
		case *journal.HuntGroup:
			text = fmt.Sprintf("group %d: %s", d.Group, coreIDs(d.Cores))
			if d.Probe != nil {
				text = fmt.Sprintf("probe %02d at %d", d.Probe.Core, d.Probe.Offset)
			}
		case *journal.HuntEnd:
			text = "hunt ends"
		case *journal.Combination:
			text = fmt.Sprintf("record C%d", d.Combination)
		case *journal.TunerDecision:
			text = fmt.Sprintf("core %02d to %d", d.Core, d.ToOffset)
		case *journal.CheckingStep:
			text = fmt.Sprintf("step %d", d.Step)
		case *journal.CorePhase:
			if d.From == journal.PhaseSearch && d.To != journal.PhaseSearch {
				text = fmt.Sprintf("solo %02d at %d", d.Core, d.Offset)
			}
		}
	}
	if text != "" {
		return "→ " + text
	}
	if t := branch.next; t != nil {
		switch {
		case t.Hunt > 0:
			return fmt.Sprintf("→ group %d", t.Group)
		case t.Rerun:
			return "→ rerun " + string(t.Regime)
		case t.Cycle > 0:
			return fmt.Sprintf("→ step %d", t.Step)
		case t.Round > 0:
			return fmt.Sprintf("→ round %d", t.Round)
		case len(t.Cores) == 0:
			return fmt.Sprintf("→ core %02d at %d", t.Core, t.Offset)
		default:
			return "→ " + string(t.Regime) + " " + coreIDs(t.Cores)
		}
	}
	return "no next trial projected"
}

func outcomeWords(branch outcome) string {
	var parts []string
	for _, decision := range forecastDecisions(branch) {
		text := decisionWords(decision)
		if branch.premise == ifNamed {
			text = namedDecisionWords(decision)
		}
		if text != "" {
			parts = append(parts, text)
		}
	}
	switch {
	case branch.needsMCE:
		parts = append(parts, "read hardware evidence before deciding")
	case branch.needsRanking:
		parts = append(parts, "read the core ranking before deciding")
	case branch.needsHistory:
		parts = append(parts, "complete hunt history needed before deciding")
	case branch.next != nil:
		parts = append(parts, "next: "+outcomeNextWords(*branch.next, branch.premise == ifNamed))
	case len(parts) == 0:
		parts = append(parts, "no next trial projected")
	}
	return strings.Join(parts, " → ")
}

func outcomeNextWords(t tuner.Trial, named bool) string {
	var position string
	switch {
	case named && len(t.Cores) == 0:
		position = "retry the named core"
	case t.Hunt > 0:
		position = fmt.Sprintf("hunt %d group %d on %s", t.Hunt, t.Group, coreIDs(t.Cores))
	case t.Rerun:
		position = "rerun on " + coreIDs(t.Cores)
	case t.Cycle > 0:
		position = fmt.Sprintf("cycle %d step %d", t.Cycle, t.Step)
	case t.Round > 0:
		position = fmt.Sprintf("round %d", t.Round)
	case len(t.Cores) == 0:
		position = fmt.Sprintf("core %02d at %d", t.Core, t.Offset)
	default:
		position = "on " + coreIDs(t.Cores)
	}
	text := position + ": " + string(t.Regime)
	if t.Workload != "" {
		text += " " + workloadDisplayID(t.Workload)
	}
	if t.RecordOnly {
		text += " · record only"
	}
	return text
}

func forecastDecisions(branch outcome) [2]journal.Payload {
	type ranked struct {
		payload      journal.Payload
		score, index int
	}
	var selected [2]ranked
	for i, payload := range branch.decisions {
		score := decisionImportance(payload)
		if score == 0 {
			continue
		}
		if (selected[0].payload != nil && selected[0].payload.Kind() == payload.Kind()) ||
			(selected[1].payload != nil && selected[1].payload.Kind() == payload.Kind()) {
			continue
		}
		candidate := ranked{payload, score, i}
		switch {
		case score > selected[0].score:
			selected[1], selected[0] = selected[0], candidate
		case score > selected[1].score:
			selected[1] = candidate
		}
	}
	if selected[1].payload != nil && selected[1].index < selected[0].index {
		selected[0], selected[1] = selected[1], selected[0]
	}
	return [2]journal.Payload{selected[0].payload, selected[1].payload}
}

func decisionImportance(payload journal.Payload) int {
	switch d := payload.(type) {
	case *journal.DeadEnd:
		return 100
	case *journal.Combination:
		return 90
	case *journal.HuntEnd:
		return 85
	case *journal.HuntStart:
		return 80
	case *journal.TunerDecision:
		return 70
	case *journal.DeepeningRound:
		return 65
	case *journal.CorePhase:
		if d.From == journal.PhaseSearch && d.To != journal.PhaseSearch {
			return 60
		}
	case *journal.CheckingCycle:
		return 50
	case *journal.CheckingStep:
		return 45
	case *journal.HuntGroup:
		return 40
	}
	return 0
}

func namedDecisionWords(payload journal.Payload) string {
	switch d := payload.(type) {
	case *journal.TunerDecision:
		switch d.Decision {
		case journal.Backoff:
			return "record its failure point; back off the named core"
		case journal.CheckSoloLimit:
			return "confirm the named core's candidate solo limit"
		case journal.StepDeeper, journal.Deepen:
			return "deepen the named core"
		case journal.Yield:
			return "the named core yields"
		}
	case *journal.CorePhase:
		if d.To == journal.PhaseSearch {
			return "restart the named core's search"
		}
		if d.To == journal.PhaseHasRoom {
			return "the named core has room"
		}
		if d.To == journal.PhaseAtLimit {
			return "the named core is at its limit"
		}
	case *journal.HuntEnd:
		return fmt.Sprintf("hunt %d ends; the named core is identified", d.Hunt)
	case *journal.DeadEnd:
		return "tuning stops at a dead end"
	case *journal.Combination:
		return fmt.Sprintf("record combination C%d", d.Combination)
	case *journal.ProfileChange:
		return "update the profile"
	default:
		return decisionWords(payload)
	}
	return ""
}

func decisionWords(payload journal.Payload) string {
	switch d := payload.(type) {
	case *journal.TunerDecision:
		verb := "moves"
		switch d.Decision {
		case journal.StepDeeper, journal.Deepen:
			verb = "goes deeper"
		case journal.Backoff:
			verb = "backs off"
		case journal.Yield:
			verb = "yields"
		case journal.CheckSoloLimit:
			return fmt.Sprintf("confirm core %02d's candidate solo limit at %d", d.Core, d.ToOffset)
		}
		text := fmt.Sprintf("core %02d %s: %d to %d", d.Core, verb, d.FromOffset, d.ToOffset)
		if d.FailurePoint != nil {
			text = fmt.Sprintf("failure point %d; core %02d %s to %d", *d.FailurePoint, d.Core, verb, d.ToOffset)
		}
		return text
	case *journal.CorePhase:
		if d.From == journal.PhaseSearch && d.To != journal.PhaseSearch {
			return fmt.Sprintf("core %02d's solo limit is %d", d.Core, d.Offset)
		}
		if d.CheckSoloLimit {
			return fmt.Sprintf("confirm core %02d at %d", d.Core, d.Offset)
		}
		if d.To == journal.PhaseHasRoom {
			return fmt.Sprintf("core %02d has room at %d", d.Core, d.Offset)
		}
		if d.To == journal.PhaseAtLimit {
			return fmt.Sprintf("core %02d is at its limit at %d", d.Core, d.Offset)
		}
		return vtText(d.Message())
	case *journal.HuntStart:
		return fmt.Sprintf("hunt %d starts over %s", d.Hunt, coreIDs(d.Candidates))
	case *journal.HuntGroup:
		text := fmt.Sprintf("hunt %d group %d: %s at failing offsets", d.Hunt, d.Group, coreIDs(d.Cores))
		if d.Probe != nil {
			text = fmt.Sprintf("probe core %02d at %d", d.Probe.Core, d.Probe.Offset)
		}
		if len(d.Held) > 0 {
			text += "; hold " + memberSummary(d.Held)
		}
		return text
	case *journal.HuntEnd:
		if d.Result == "direct" || d.Result == "culprit" {
			return fmt.Sprintf("hunt %d ends; %s identified", d.Hunt, coreIDs(d.Cores))
		}
		return fmt.Sprintf("hunt %d ends: %s over %s", d.Hunt, vtText(d.Result), coreIDs(d.Cores))
	case *journal.Combination:
		return fmt.Sprintf("record C%d over %s", d.Combination, memberCoreIDs(d.Members))
	case *journal.CheckingStep:
		return fmt.Sprintf("cycle %d advances to step %d", d.Cycle, d.Step)
	case *journal.CheckingCycle:
		if d.Event == journal.CycleStart {
			return fmt.Sprintf("cycle %d starts", d.Cycle)
		}
		if d.Passed && d.Full {
			return fmt.Sprintf("cycle %d passed with full coverage", d.Cycle)
		}
		if d.Passed {
			return fmt.Sprintf("cycle %d passed; missing %s", d.Cycle, strings.Join(d.Missing, ", "))
		}
		return fmt.Sprintf("cycle %d ends: %s", d.Cycle, vtText(d.Reason))
	case *journal.DeepeningRound:
		if d.Event == journal.CycleStart {
			return fmt.Sprintf("deepening round %d starts over %s", d.Round, coreIDs(d.Cores))
		}
		if d.Passed {
			return fmt.Sprintf("deepening round %d passed", d.Round)
		}
		return fmt.Sprintf("deepening round %d ends: %s", d.Round, vtText(d.Reason))
	case *journal.DeadEnd:
		return "dead end: " + strings.ReplaceAll(vtText(string(d.Condition)), "_", " ")
	case *journal.TunerWarning:
		return vtText(d.Message())
	case *journal.HostRanking:
		return "core ranking recorded"
	case *journal.HuntSkipped:
		return "no hunt: " + vtText(d.Reason)
	}
	return ""
}

func memberSummary(members []journal.CombinationMember) string {
	if len(members) <= 2 {
		return compactMembers(members)
	}
	return memberCoreIDs(members) + " at recorded offsets"
}

func compactMembers(members []journal.CombinationMember) string {
	parts := make([]string, len(members))
	for i, member := range members {
		parts[i] = fmt.Sprintf("%02d:%d", member.Core, member.Offset)
	}
	return strings.Join(parts, " ")
}

func memberCoreIDs(members []journal.CombinationMember) string {
	ids := make([]int, len(members))
	for i, member := range members {
		ids[i] = member.Core
	}
	return coreIDs(ids)
}

func workloadDisplay(w machine.Workload) string {
	if w.Base != "" {
		if base, ok := machine.WorkloadByID(w.Base); ok {
			return vtText(base.Label)
		}
	}
	if w.Label != "" {
		return vtText(w.Label)
	}
	return vtText(w.ID)
}

func workloadDisplayID(id string) string {
	if w, ok := machine.WorkloadByID(id); ok {
		return workloadDisplay(w)
	}
	return vtText(id)
}
