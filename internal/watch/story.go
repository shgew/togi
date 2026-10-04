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
		text := "The trial's intent is recorded; its offsets are being applied and its workload is starting."
		return story{s.stageLabel(), []string{text}, text, plainTone}
	}
	if s.hunt != nil {
		return s.huntStory()
	}
	if t.condition == machine.Alone && t.round == 0 {
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
		label := fmt.Sprintf("DEEPEN · ROUND %d", t.round)
		if t.condition == machine.Alone {
			text := fmt.Sprintf("Core %02d runs alone at its proposed %d, like every deepened core's light and heavy checks.", t.core, t.offset)
			return story{label, []string{text, "R7 then checks the parts where a deepened core is a top requester."}, fmt.Sprintf("Deepening check: core %02d alone at its proposed %d.", t.core, t.offset), plainTone}
		}
		return story{label, []string{"These offsets passed a full cycle. I check the deepening plan's proposed profile together.", "The checks below distinguish cores that go deeper from members that yield shallower."}, "Checking the deepening plan's proposed profile.", plainTone}
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
	if t.partial && !t.recordOnly {
		where = partialNote
		brief = fmt.Sprintf("Step %d, R7 partial loads %s; passes and failures count.", t.step, coreIDs(t.cores))
	}
	if s.cleanCycles > 0 && !t.partial {
		where = fmt.Sprintf("%d clean cycles count for this profile. Passing trials cannot prove it will never fail.", s.cleanCycles)
	}
	return story{name, []string{step, where}, brief, plainTone}
}

const partialNote = "This partial idles the top-requester groups found so far. Its loaded set freezes when the part starts, even if offsets change. Passes and failures count as ordinary evidence."

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
		var signal machine.Signal
		for _, g := range h.groups {
			if g.probe == nil {
				group = g.cores
				if g.outcome == "failure" {
					signal = g.signal
				}
			}
		}
		if len(h.probes) > 0 {
			group = group[:0:0]
			for _, probe := range h.probes {
				group = append(group, probe.member)
			}
		}
		// The retained group's own failure, not the trial that started the hunt.
		verb := "failed"
		if signal == machine.Crash {
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
	for _, part := range h.plan {
		if part.running {
			// The part parks every core outside it, candidates or not: say "at 0" only when the profile running
			// holds each of them there.
			parked := "parked"
			if t := s.trial; t != nil && s.allAtZero(t.profile, part.parked) {
				parked = "parked at 0"
			}
			return fmt.Sprintf("%s at their failing offsets, %s %s", coreIDs(part.failing), coreIDs(part.parked), parked)
		}
	}
	return fmt.Sprintf("part of %s at its offsets, the rest %s", coreIDs(h.candidates), parked)
}

// allAtZero reports whether profile, in session core order, holds every listed core at 0.
func (s Snapshot) allAtZero(profile, cores []int) bool {
	for _, id := range cores {
		i := slices.Index(s.order, id)
		if i < 0 || i >= len(profile) || profile[i] != 0 {
			return false
		}
	}
	return true
}

// knownWords describes a hunt started by a trial skipped because its profile already failed.
func (c huntCause) knownWords() string {
	text := fmt.Sprintf("%s %s already failed at these offsets", vtText(string(c.regime)), kindWords(c.regime))
	if c.regime == "" {
		text = "an idle failure is already recorded at these offsets"
		if c.carried {
			text += ", carried from an earlier session"
		}
		return text
	}
	if c.carried {
		text += " in a carried trial"
	}
	return text
}

// huntCauseStory tells what started the hunt: a sentence, whether a core was named, and a compact line.
func (s Snapshot) huntCauseStory() (string, string, string) {
	c := s.hunt.cause
	named := "No core was named: "
	if c.core != nil {
		named = fmt.Sprintf("Core %02d was named: ", *c.core)
	}
	if c.known {
		text := c.knownWords() + "."
		return text, named, text
	}
	if c.trial.regime == "" {
		// An idle failure between trials starts a hunt without a failed trial.
		return "The machine crashed while idle, with no trial running, and rebooted.", named, "Crashed while idle; no core named."
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
	case t.condition == machine.Alone && t.round == 0:
		verb := "SEARCH"
		if c := s.core(t.core); c != nil && c.confirm != nil {
			verb = "CONFIRM"
		}
		text = fmt.Sprintf("%s CORE %02d AT %d", verb, t.core, t.offset) + loadWord(t.regime)
	case t.rerun:
		text = "RERUN AFTER BACKOFF"
	case t.round > 0:
		text = fmt.Sprintf("DEEPEN · ROUND %d", t.round)
		if t.condition == machine.Alone {
			text += fmt.Sprintf(" · CORE %02d AT %d", t.core, t.offset) + loadWord(t.regime)
		}
	default:
		text = fmt.Sprintf("CYCLE %d · STEP %d", t.cycle, t.step)
		if t.parts > 1 {
			text += fmt.Sprintf(" · PART %d", t.part)
		}
	}
	if t.recordOnly {
		text += " · RECORD ONLY"
	} else if t.partial {
		text += " · PARTIAL"
	}
	return tone.Render(text)
}

// loadWord marks an alone trial's light or heavy load.
func loadWord(regime machine.Regime) string {
	switch regime {
	case machine.R1:
		return " · LIGHT"
	case machine.R2:
		return " · HEAVY"
	case machine.R3, machine.R4, machine.R5, machine.R6, machine.R7:
	}
	return ""
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
		// One loaded core: alone only in search; otherwise every other core keeps its profile offset.
		what = fmt.Sprintf("%s %s on core %02d at %d", n.Regime, kindWords(n.Regime), n.Core, n.Offset)
		if n.Condition == machine.Alone {
			what = fmt.Sprintf("%s %s on core %02d alone at %d", n.Regime, kindWords(n.Regime), n.Core, n.Offset)
		}
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

// partWords names the part of the current checking step a trial loads: "part 2: full CCD 0 on 00-07, 5m", or
// "part 4: core 09 alone at -26, 2m" for a step that loads one core at a time.
func (s Snapshot) partWords(n tuner.Trial) string {
	g := s.cycle
	if g == nil || n.Step < 1 || n.Step > len(g.steps) {
		return ""
	}
	step := g.steps[n.Step-1]
	duration := shortDuration(time.Duration(n.DurationS) * time.Second)
	want := slices.Sorted(slices.Values(n.Cores))
	if len(want) == 0 {
		want = []int{n.Core}
	}
	for i, part := range step.parts {
		if !slices.Equal(slices.Sorted(slices.Values(part.cores)), want) {
			continue
		}
		if len(n.Cores) == 0 {
			return fmt.Sprintf("part %d: core %02d at %d, %s", i+1, n.Core, n.Offset, duration)
		}
		return fmt.Sprintf("part %d: %s on %s, %s", i+1, partName(step.regime, part), coreIDs(n.Cores), duration)
	}
	return ""
}

// sameTrial is true when n is another trial of the same requirement as t.
func sameTrial(n tuner.Trial, t *trialView) bool {
	if t == nil {
		return false
	}
	same := n.Regime == t.regime && n.Workload == t.workload.ID && time.Duration(n.DurationS)*time.Second == t.duration &&
		n.Hunt == t.hunt && n.Group == t.group && n.Cycle == t.cycle && n.Step == t.step && n.Round == t.round &&
		n.Rerun == t.rerun
	if len(n.Cores) == 0 {
		return same && t.condition == machine.Alone && n.Core == t.core && n.Offset == t.offset
	}
	return same && slices.Equal(slices.Sorted(slices.Values(n.Cores)), slices.Sorted(slices.Values(t.cores)))
}

// samePart is true when n is a later trial of the checking part t belongs to, perhaps of another length.
func samePart(n tuner.Trial, t *trialView) bool {
	return t != nil && n.Cycle > 0 && n.Hunt == 0 && !n.Rerun && n.Cycle == t.cycle && n.Step == t.step &&
		slices.Equal(slices.Sorted(slices.Values(n.Cores)), slices.Sorted(slices.Values(t.cores)))
}

// phrase is one step of an outcome line; a minor one is left out first when the line does not fit.
type phrase struct {
	text  string
	minor bool
}

type outcomeRow struct {
	label, short, compact string
	phrases               []phrase
	style                 lipgloss.Style
	basis                 string // where backoffs take top requesters from, when new telemetry could change them
}

// fitPhrases joins an outcome line's phrases with arrows, leaving out minor ones, then any but the first and the
// last, until it fits.
func fitPhrases(phrases []phrase, width int) string {
	join := func(ps []phrase) string {
		parts := make([]string, len(ps))
		for i, p := range ps {
			parts[i] = consoleText(p.text)
		}
		return strings.Join(parts, " → ")
	}
	text := join(phrases)
	for ansi.StringWidth(text) > width {
		drop := -1
		for i, p := range phrases {
			if p.minor && p.text != "..." {
				drop = i
				break
			}
		}
		if drop < 0 {
			for i := 1; i < len(phrases)-1; i++ {
				if phrases[i].text != "..." {
					drop = i
					break
				}
			}
		}
		if drop < 0 {
			return text
		}
		phrases = slices.Clone(phrases)
		switch {
		case drop > 0 && phrases[drop-1].text == "...":
			phrases = slices.Delete(phrases, drop, drop+1)
		case drop+1 < len(phrases) && phrases[drop+1].text == "...":
			phrases = slices.Delete(phrases, drop, drop+1)
		default:
			phrases[drop] = phrase{"...", true}
		}
		text = join(phrases)
	}
	return text
}

// outcomeRows are the outcome lines: each branch's decisions in words, branches that say the same merged into one.
func (s Snapshot) outcomeRows() []outcomeRow {
	t := s.trial
	type group struct {
		premises []premise
		phrases  []phrase
		text     string
		compact  string
		basis    string
		passes   int
		zero     *int // the core at 0 a named branch follows, unless the group also holds the stand-in
		standIn  bool // holds the named branch that stands in for every core away from 0
	}
	var groups []group
	for _, branch := range s.outcomes {
		phrases, compact, basis := s.outcomeWords(branch)
		text := fitPhrases(phrases, 1<<20)
		i := slices.IndexFunc(groups, func(g group) bool { return g.text == text })
		if i < 0 {
			groups = append(groups, group{phrases: phrases, text: text, compact: compact, basis: basis})
			i = len(groups) - 1
		}
		groups[i].premises = append(groups[i].premises, branch.premise)
		groups[i].passes = max(groups[i].passes, branch.passes)
		if branch.premise == ifNamed {
			if branch.atZero {
				groups[i].zero = branch.core
			} else {
				groups[i].standIn = true
			}
		}
	}
	hasAll := slices.ContainsFunc(groups, func(g group) bool { return slices.Contains(g.premises, ifAllPass) })
	if hasAll {
		groups = slices.DeleteFunc(groups, func(g group) bool { return slices.Equal(g.premises, []premise{ifPasses}) })
	}
	standIn := slices.IndexFunc(groups, func(g group) bool { return g.standIn })
	zero := slices.IndexFunc(groups, func(g group) bool {
		return g.zero != nil && slices.Equal(g.premises, []premise{ifNamed}) && len(g.phrases) > 0
	})
	visible := len(groups)
	if slices.ContainsFunc(groups, func(g group) bool { return slices.Equal(g.premises, []premise{ifInconclusive}) }) {
		visible--
	}
	if standIn >= 0 && zero >= 0 && len(groups[standIn].phrases) > 0 && visible > outcomeRowLimit {
		// Too few rows for a line of its own: the core at 0 becomes an exception on the line of the other cores.
		z, g := groups[zero], &groups[standIn]
		exception := "a core at 0:" + strings.TrimPrefix(z.compact, "→")
		g.phrases = slices.Clone(g.phrases)
		g.phrases[len(g.phrases)-1].text += " · " + exception
		g.compact += " · " + exception
		groups = slices.Delete(groups, zero, zero+1)
	}
	namedGroups := 0
	for _, g := range groups {
		if slices.Contains(g.premises, ifNamed) {
			namedGroups++
		}
	}
	order := []premise{ifAllPass, ifPasses, ifNamed, ifUnnamed, ifInconclusive}
	if s.hunt != nil {
		order = []premise{ifAllPass, ifPasses, ifUnnamed, ifNamed, ifInconclusive}
	}
	rank := func(g group) int {
		best := len(order)
		for _, p := range g.premises {
			best = min(best, slices.Index(order, p))
		}
		return best
	}
	slices.SortStableFunc(groups, func(a, b group) int { return rank(a) - rank(b) })
	var out []outcomeRow
	for _, g := range groups {
		who := "a core"
		if namedGroups > 1 {
			who = "another core"
			if g.zero != nil && !g.standIn {
				who = "a core at 0"
			}
		}
		label, short, style := premiseWords(g.premises, g.passes, t, who)
		out = append(out, outcomeRow{label, short, g.compact, g.phrases, style, g.basis})
		if len(out) == outcomeRowLimit {
			break
		}
	}
	return out
}

// outcomeRowLimit is how many outcome lines the NOW band holds.
const outcomeRowLimit = 3

// premiseWords labels an outcome line; who names the core a named failure is about.
func premiseWords(premises []premise, passes int, t *trialView, who string) (string, string, lipgloss.Style) {
	has := func(p premise) bool { return slices.Contains(premises, p) }
	fails := has(ifNamed) || has(ifUnnamed)
	pass := has(ifPasses) || has(ifAllPass)
	switch {
	case pass && fails:
		return "pass or fail", "pass or fail", textStyle
	case has(ifAllPass):
		passed, of := passes, passes
		if t != nil && t.of > 0 {
			passed, of = t.passed+passes, t.of
		}
		return fmt.Sprintf("if %d of %d pass", passed, of), fmt.Sprintf("%d/%d pass", passed, of), green
	case has(ifPasses):
		return "if it passes", "pass", green
	case has(ifNamed) && has(ifUnnamed):
		return "if it fails", "fail", red
	case has(ifNamed):
		return "if " + who + " is named", strings.Replace(strings.TrimPrefix(who, "a "), "another", "other", 1) + " named", red
	case has(ifUnnamed):
		return "if none is named", "none named", red
	}
	return "if inconclusive", "inconclusive", grey
}

// outcomeWords puts a branch's decisions in words, in the order the tuner records them, then the trial it runs next.
// basis says where the decisions take top requesters from when the forecast end has no telemetry.
func (s Snapshot) outcomeWords(branch outcome) (phrases []phrase, compact, basis string) {
	t := s.trial
	named := branch.premise == ifNamed
	phrases = s.decisionPhrases(branch.decisions, named)
	if branch.withoutTelemetry && len(phrases) > 0 {
		basis = s.requestBasis()
		phrases[len(phrases)-1].text += " " + basis
	}
	var shape huntShape
	stepDone := false
	for _, d := range branch.decisions {
		switch d := d.(type) {
		case *journal.HuntStart:
			shape = huntShape{d.Failing, d.Parked}
		case *journal.CheckingStep:
			stepDone = true
		}
	}
	passing := branch.premise == ifPasses || branch.premise == ifAllPass
	switch {
	case t == nil || !passing || stepDone:
	case t.step > 0 && branch.next != nil && branch.next.Cycle == t.cycle && branch.next.Step > t.step:
		phrases = append(phrases, phrase{fmt.Sprintf("step %d is done", t.step), false})
	case t.parts > 1 && t.of > 0 && t.passed+branch.passes >= t.of:
		phrases = append(phrases, phrase{fmt.Sprintf("part %d is done", t.part), false})
	}
	if t != nil && t.recordOnly && len(phrases) == 0 && branch.premise != ifInconclusive {
		phrases = append(phrases, phrase{"recorded only, moves nothing", false})
	}
	for _, d := range branch.decisions {
		switch d := d.(type) {
		case *journal.DeadEnd:
			compact = "tuning stops"
		case *journal.HuntEnd:
			if compact == "" {
				compact = "hunt ends"
			}
		case *journal.HuntStart:
			if compact == "" {
				compact = fmt.Sprintf("hunt %d", d.Hunt)
			}
		case *journal.Combination:
			if compact == "" {
				compact = fmt.Sprintf("C%d", d.Combination)
			}
		case *journal.TunerDecision:
			if compact == "" && d.Decision == journal.Backoff {
				compact = "backs off"
			}
		}
	}
	var next string
	switch {
	case branch.needsRanking:
		next = "read the core ranking before deciding"
	case branch.needsHistory:
		next = "complete hunt history needed before deciding"
	case branch.next == nil:
		if len(phrases) == 0 {
			next = "no next trial projected"
		}
	case branch.next.Retry && sameTrial(*branch.next, t):
		next, compact = "the same trial runs again", "same trial again"
	case sameTrial(*branch.next, t) && (branch.premise == ifPasses || t.recordOnly) && t.index < t.of:
		next = fmt.Sprintf("next: trial %d of %d", t.index+1, t.of)
	case sameTrial(*branch.next, t):
		next = "next: this load again"
	case samePart(*branch.next, t) && (passing || t.recordOnly) && t.index+max(branch.passes, 1) <= t.of:
		next = fmt.Sprintf("next: trial %d of %d · %s", t.index+max(branch.passes, 1), t.of, shortDuration(time.Duration(branch.next.DurationS)*time.Second))
	default:
		next = "next: " + s.nextTrialWords(*branch.next, t, shape)
	}
	if compact == "" {
		compact = compactNext(next)
	}
	if next != "" {
		phrases = append(phrases, phrase{next, false})
	}
	return phrases, "→ " + compact, basis
}

// compactNext shortens what comes next to its position: "part 2: 08-15", "part 4: core 09", "group 8", "step 3".
func compactNext(next string) string {
	text := strings.TrimPrefix(next, "next: ")
	if name, rest, ok := strings.Cut(text, ": "); ok {
		if strings.HasPrefix(name, "hunt ") {
			_, group, _ := strings.Cut(name, " group ")
			return "group " + group
		}
		if strings.HasPrefix(name, "part ") {
			if core, ok := strings.CutPrefix(rest, "core "); ok {
				id, _, _ := strings.Cut(core, " ")
				return name + ": core " + id
			}
			cores, _, _ := strings.Cut(rest, " ")
			if strings.Contains(rest, " on ") {
				return name
			}
			return name + ": " + cores
		}
		return name
	}
	if strings.HasPrefix(text, "rerun ") {
		return "rerun"
	}
	return text
}

// decisionPhrases puts each recorded decision in a short phrase. Consecutive hunt groups answered by carried trials
// become one phrase. A branch that assumes a named core speaks of "it", since any loaded core could be the one.
func (s Snapshot) decisionPhrases(decisions []journal.Payload, named bool) []phrase {
	var out []phrase
	push := func(text string, minor bool) {
		if text != "" && (len(out) == 0 || out[len(out)-1].text != text) {
			out = append(out, phrase{text, minor})
		}
	}
	add := func(text string) { push(text, false) }
	minor := func(text string) { push(text, true) }
	var namedCore, failed *int
	for i := 0; i < len(decisions); i++ {
		switch d := decisions[i].(type) {
		case *journal.Failure:
			switch {
			case d.KnownFailure != 0:
				add(s.knownFailureWords(d, decisions[i+1:]))
			case named && d.Core != nil:
				namedCore = d.Core
				add("its failure point is recorded")
			case d.Core != nil:
				failed = d.Core
			}
		case *journal.HuntStart:
			minor(fmt.Sprintf("hunt %d starts over %s", d.Hunt, coreIDs(d.Candidates)))
		case *journal.HuntGroup:
			if d.Skipped {
				add(fmt.Sprintf("group %d skipped", d.Group))
				continue
			}
			if d.Inferred == "" {
				continue
			}
			var p phrase
			p, i = carriedGroups(decisions, i)
			push(p.text, p.minor)
		case *journal.HuntEnd:
			push(huntEndPhrase(d, named))
		case *journal.Combination:
			add(fmt.Sprintf("C%d over %s", d.Combination, memberCoreIDs(d.Members)))
		case *journal.TunerDecision:
			if named && namedCore != nil && d.Core == *namedCore {
				add("it backs off")
				continue
			}
			add(decisionPhrase(d, failed))
		case *journal.CorePhase:
			switch {
			case d.From == journal.PhaseSearch && d.To != journal.PhaseSearch:
				add(fmt.Sprintf("core %02d solo limit %d", d.Core, d.Offset))
			case named:
			case d.To == journal.PhaseSearch:
				add(fmt.Sprintf("core %02d starts its search over", d.Core))
			}
		case *journal.CheckingStep:
			if d.Step > 1 {
				add(fmt.Sprintf("step %d is done", d.Step-1))
			}
		case *journal.CheckingCycle:
			switch {
			case d.Event == journal.CycleStart:
				minor(fmt.Sprintf("cycle %d starts", d.Cycle))
			case d.Passed && d.Full:
				add(fmt.Sprintf("cycle %d passed, a full cycle", d.Cycle))
			case d.Passed:
				add(fmt.Sprintf("cycle %d passed", d.Cycle))
			default:
				add(fmt.Sprintf("cycle %d ends", d.Cycle))
			}
		case *journal.DeepeningRound:
			switch {
			case d.Event == journal.CycleStart:
				add(fmt.Sprintf("deepening round %d starts", d.Round))
			case d.Passed:
				add(fmt.Sprintf("deepening round %d passed", d.Round))
			default:
				add(fmt.Sprintf("deepening round %d ends", d.Round))
			}
		case *journal.DeadEnd:
			add("tuning stops: " + strings.ReplaceAll(vtText(string(d.Condition)), "_", " "))
		case *journal.HuntSkipped:
			add("no hunt: these offsets reach a known failure")
		}
	}
	return out
}

// carriedGroups folds the run of hunt groups answered by carried trials that starts at decisions[i] into one phrase,
// and returns the index of the run's last group.
func carriedGroups(decisions []journal.Payload, i int) (phrase, int) {
	d := decisions[i].(*journal.HuntGroup)
	last := i
	for last+1 < len(decisions) {
		g, ok := decisions[last+1].(*journal.HuntGroup)
		if !ok || g.Inferred != d.Inferred || g.Skipped || g.Hunt != d.Hunt {
			break
		}
		last++
	}
	first, end := d.Group, decisions[last].(*journal.HuntGroup).Group
	groups := fmt.Sprintf("group %d", first)
	if end > first {
		groups = fmt.Sprintf("groups %d-%d", first, end)
	}
	switch {
	case d.Inferred != "pass":
		return phrase{groups + " failed on existing evidence", false}, last
	case end > first:
		return phrase{groups + " answered by existing evidence", true}, last
	}
	return phrase{groups + " answered by existing evidence", true}, last
}

func huntEndPhrase(d *journal.HuntEnd, named bool) (string, bool) {
	switch {
	case named && (d.Result == "direct" || d.Result == "culprit"):
		return "the hunt ends", false
	case d.Result == "direct" || d.Result == "culprit":
		return fmt.Sprintf("hunt %d ends: %s named", d.Hunt, coreIDs(d.Cores)), false
	case d.Result == "fallback":
		return fmt.Sprintf("hunt %d unresolved", d.Hunt), true
	case d.Result == "combination":
		return fmt.Sprintf("hunt %d keeps %s together", d.Hunt, coreIDs(d.Cores)), false
	case d.Result == "cancelled":
		return fmt.Sprintf("hunt %d cancelled", d.Hunt), false
	}
	return fmt.Sprintf("hunt %d ends", d.Hunt), false
}

// decisionPhrase puts a move of one core in words; failed is the core a failure in the same branch named, whose
// backoff records its failure point.
func decisionPhrase(d *journal.TunerDecision, failed *int) string {
	switch d.Decision {
	case journal.Backoff:
		if failed != nil && *failed == d.Core && d.FailurePoint != nil {
			return fmt.Sprintf("core %02d fails at %d, %d → %d", d.Core, *d.FailurePoint, d.FromOffset, d.ToOffset)
		}
		return fmt.Sprintf("core %02d %d → %d", d.Core, d.FromOffset, d.ToOffset)
	case journal.StepDeeper:
		return fmt.Sprintf("core %02d next %d", d.Core, d.ToOffset)
	case journal.CheckSoloLimit:
		return fmt.Sprintf("core %02d confirms %d", d.Core, d.ToOffset)
	case journal.Deepen:
		return fmt.Sprintf("core %02d deepens %d → %d", d.Core, d.FromOffset, d.ToOffset)
	case journal.Yield:
		return fmt.Sprintf("core %02d yields %d → %d", d.Core, d.FromOffset, d.ToOffset)
	}
	return ""
}

// knownFailureWords says which trial the tuner skips because it already failed at these offsets.
func (s Snapshot) knownFailureWords(d *journal.Failure, after []journal.Payload) string {
	what := string(d.Regime) + " " + kindWords(d.Regime)
	for _, p := range after {
		start, ok := p.(*journal.HuntStart)
		if !ok {
			continue
		}
		what += " on " + coreIDs(start.Cores)
		if t := s.trial; t != nil && !t.rerun {
			if name := s.partWords(tuner.Trial{Step: t.step, Cores: start.Cores}); name != "" {
				_, rest, _ := strings.Cut(name, ": ")
				what, _, _ = strings.Cut(rest, " on ")
			}
		}
		break
	}
	verb := "failed"
	if d.Signal == machine.Crash {
		verb = "crashed"
	}
	text := what + " at these offsets already " + verb
	if s.carried[d.KnownFailure] {
		text += " in a carried trial"
	}
	return text
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
