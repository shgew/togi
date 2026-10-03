package watch

import (
	"fmt"
	"slices"
	"strings"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

// story is what togi says about the moment: a headline, a few paragraphs and the test running now.
type story struct {
	headline   string
	tone       tone
	paragraphs []string
	now        *nowLine
}

type nowLine struct {
	what     string
	detail   string
	backend  string
	progress float64
	timed    bool
	left     time.Duration
}

func (s Snapshot) story(now time.Time) story {
	switch {
	case s.problem != nil:
		return story{headline: "I CAN'T READ THE JOURNAL", tone: badTone, paragraphs: []string{
			vtText(s.problem.Error()),
			"I keep trying every second. Tuning itself is not affected by this screen.",
		}}
	case !s.session:
		return story{headline: "NO SESSION YET", paragraphs: []string{
			"Nothing has been recorded yet. Start tuning with togi run, or boot the togi entry, and I'll tell you what I'm doing here.",
		}}
	case s.deadEnd != nil:
		return s.deadEndStory()
	case s.stopped != nil:
		st := story{headline: "I'M STOPPED", paragraphs: []string{
			"I put the offsets back to safe values before stopping. The numbers below are what I found, not what is applied now. Everything I learned is in the journal, and togi run picks up where I left off.",
		}}
		if s.goal() {
			st.paragraphs = append(st.paragraphs, "These offsets passed a clean lap of every kind of test: they are the ones to carry into the BIOS.")
		}
		return st
	}
	var st story
	t := s.trial
	switch {
	case t == nil:
		st = s.idleStory()
	case !t.hasStarted:
		st = story{headline: s.headline(), tone: plainTone, paragraphs: []string{
			"I'm preparing the next test. Its intent is recorded, but the workload hasn't started yet.",
			fmt.Sprintf("Planned: %s on %s for %s.", regimeWords[t.regime], coresText(t.cores, len(s.cores)), duration(t.duration)),
		}}
	case t.condition == machine.Isolated:
		st = s.searchStory(t)
	case t.condition == machine.Masked:
		st = s.huntStory(t, now)
	case t.round != 0:
		st = s.deepenStory()
	case t.rerun:
		st = s.rerunStory()
	default:
		st = s.lapStory(t)
	}
	if t != nil {
		st.now = s.nowLine(t, now)
	}
	if s.lastCrash != nil && st.headline != "FINDING THE CULPRIT" && now.Sub(*s.lastCrash) < 30*time.Minute && (s.lastFailure == nil || !s.lastFailure.at.Before(*s.lastCrash)) {
		lead := fmt.Sprintf("The machine crashed and rebooted %s, and I picked up where I left off.", ago(now.Sub(*s.lastCrash)))
		st.paragraphs = append([]string{lead}, st.paragraphs...)
	}
	return st
}

func (s Snapshot) goal() bool {
	return s.guard != nil && s.guard.CleanRotations > 0 && s.phase == journal.PhaseGuard &&
		!s.refinable && s.refine == nil && s.rerunDuration == 0 && len(s.cores) > 0 &&
		!slices.ContainsFunc(s.cores, func(c coreView) bool { return c.phase != journal.PhaseDone || c.queued })
}

// currentStep is the number of the lap step running now, counting from 1.
func currentStep(g *journal.GuardState) int {
	return min(g.StepsDone+1, len(g.Steps))
}

func (s Snapshot) searching() int {
	n := 0
	for _, c := range s.cores {
		if c.phase == journal.PhaseSearch {
			n++
		}
	}
	return n
}

func (s Snapshot) core(id int) coreView {
	for _, c := range s.cores {
		if c.id == id {
			return c
		}
	}
	return coreView{id: id}
}

func (s Snapshot) idleStory() story {
	st := story{headline: s.headline(), tone: plainTone}
	if s.inFlight != "" {
		st.paragraphs = append(st.paragraphs, "Between tests: "+s.inFlight)
	} else {
		st.paragraphs = append(st.paragraphs, "Deciding what to test next.")
	}
	return st
}

func (s Snapshot) headline() string {
	switch {
	case s.phase == journal.PhaseSearch:
		return "FINDING LIMITS"
	case s.phase == journal.PhaseHunt:
		return "FINDING THE CULPRIT"
	case s.phase == journal.PhaseRefine:
		return "GOING DEEPER"
	case s.goal():
		return "KEEPING WATCH"
	}
	return "TESTING TOGETHER"
}

func (s Snapshot) searchStory(t *trial) story {
	if len(t.cores) == 0 {
		return s.idleStory()
	}
	c := s.core(t.cores[0])
	at := c.applied
	if t.offset != nil {
		at = *t.offset
	}
	st := story{headline: "FINDING LIMITS", tone: plainTone}
	st.paragraphs = append(st.paragraphs, fmt.Sprintf(
		"I'm finding each core's limit, one core at a time. Core %02d runs a %s alone at %d while every other core waits at 0, so a failure can only be its own.",
		c.id, regimeWords[t.regime], at))
	switch {
	case c.checking:
		st.paragraphs = append(st.paragraphs, fmt.Sprintf(
			"%d looks like its limit, so I'm confirming it: it has to pass %d light and %d heavy runs in a row before I trust it.", at, s.starts, s.starts))
	case c.pass == nil && c.fail == nil:
		st.paragraphs = append(st.paragraphs, "This is its first step. Each step is a light load, then a heavy vector load, at the same offset. If both pass, it goes 5 counts deeper.")
	case c.fail == nil:
		st.paragraphs = append(st.paragraphs, fmt.Sprintf("So far it passed down to %d and hasn't failed yet. If this step passes, it goes 5 counts deeper.", *c.pass))
	case c.pass == nil:
		st.paragraphs = append(st.paragraphs, fmt.Sprintf("It failed at %d, so I'm backing up 5 counts at a time until it passes.", *c.fail))
	default:
		st.paragraphs = append(st.paragraphs, fmt.Sprintf(
			"It passes at %d and fails at %d, so its limit is between %d and %d. I'm closing in one count at a time.", *c.pass, *c.fail, *c.pass, *c.fail+1))
	}
	left := s.searching()
	st.paragraphs = append(st.paragraphs, fmt.Sprintf("%d of %d cores have found their limit, %d to go.", len(s.cores)-left, len(s.cores), left))
	return st
}

func (s Snapshot) huntStory(t *trial, now time.Time) story {
	st := story{headline: "FINDING THE CULPRIT", tone: warnTone}
	h := s.hunt
	if h == nil {
		st.paragraphs = append(st.paragraphs, "A test failed with every core at its offset and nothing named a single core, so I'm testing groups of cores to find out which ones cause it.")
		return st
	}
	st.paragraphs = append(st.paragraphs, causeText(h, len(s.cores), now))
	m := h.mask
	if m == nil {
		return st
	}
	suspects := coreList(m.cores)
	switch m.stage {
	case "full":
		st.paragraphs = append(st.paragraphs,
			"Every smaller group passed on its own, so I'm rerunning the whole failing set of offsets to see whether it fails again.",
			"If it fails, the cause needs several cores deep at once and I keep narrowing. If it passes, I repeat it at the length of the original test.")
	case "edge":
		if m.edge != nil {
			combination := slices.Clone(m.cores)
			if !slices.Contains(combination, m.edge.Core) {
				combination = append(combination, m.edge.Core)
			}
			st.paragraphs = append(st.paragraphs,
				fmt.Sprintf("%s fail only together. Now I'm finding how far core %02d must back off for them to pass: it runs at %d while the others stay at their failing offsets.", capital(coreList(combination)), m.edge.Core, m.edge.Offset),
				fmt.Sprintf("If this passes %d times, core %02d is safe at %d in that combination. If it fails, it has to back off further.", m.needed, m.edge.Core, m.edge.Offset))
		}
	default:
		back := "back at the offsets they failed with"
		outcome := fmt.Sprintf("If this test fails, the cause is among %s and I split them further.", suspects)
		if len(m.cores) == 1 {
			back = "back at the offset it failed with"
			outcome = fmt.Sprintf("If this test fails, %s is the culprit.", suspects)
		}
		st.paragraphs = append(st.paragraphs,
			fmt.Sprintf("So I'm narrowing it down. %s %s %s. %s.", capital(suspects), are(m.cores), back, capital(s.parkedText(h, m))))
		if idle := slices.DeleteFunc(slices.Clone(m.cores), func(c int) bool { return slices.Contains(t.cores, c) }); len(idle) > 0 {
			pronoun := "they are"
			if len(idle) == 1 {
				pronoun = "it is"
			}
			st.paragraphs = append(st.paragraphs, fmt.Sprintf("The load runs on %s, as in the test that failed. %s %s idle, but had these offsets when it failed, so %s under suspicion too.",
				coresText(t.cores, len(s.cores)), capital(coreList(idle)), are(idle), pronoun))
		}
		st.paragraphs = append(st.paragraphs, fmt.Sprintf("%s If it passes %d times, I try a different group next.", outcome, m.needed))
	}
	return st
}

func (s Snapshot) parkedText(h *huntView, m *maskView) string {
	var parkedCores []int
	allZero := true
	for i, c := range s.cores {
		if slices.Contains(h.candidates, c.id) && !slices.Contains(m.cores, c.id) {
			parkedCores = append(parkedCores, c.id)
			if i < len(h.anchor) && h.anchor[i] != 0 {
				allZero = false
			}
		}
	}
	if len(parkedCores) == 0 {
		return "every other core keeps its offset"
	}
	where := "at offsets that passed before"
	if allZero {
		where = "at 0"
	}
	pronoun := "they"
	if len(parkedCores) == 1 {
		pronoun = "it"
	}
	return fmt.Sprintf("%s %s parked %s, so %s can't be the cause", coreList(parkedCores), are(parkedCores), where, pronoun)
}

func causeText(h *huntView, total int, now time.Time) string {
	what := "A test failed"
	when := ""
	if c := h.cause; c != nil {
		what = capital(failureCause(c.signal))
		when = " " + ago(now.Sub(c.at))
	}
	return fmt.Sprintf("%s%s during %s on %s, and nothing in it names a single core.", what, when, article(regimeWords[h.regime]), coresText(h.loaded, total))
}

func failureCause(sig machine.Signal) string {
	switch sig {
	case machine.Crash:
		return "the machine crashed"
	case machine.ComputationError:
		return "a stress test got a wrong result"
	case machine.Stall:
		return "a stress test stalled"
	case machine.UnexpectedExit:
		return "a stress test quit unexpectedly"
	case machine.CorrectedMCE:
		return "the CPU reported a corrected hardware error"
	case machine.UncorrectedMCE:
		return "the CPU reported an uncorrected hardware error"
	}
	return "a test failed"
}

func (s Snapshot) deepenStory() story {
	st := story{headline: "GOING DEEPER", tone: plainTone}
	st.paragraphs = append(st.paragraphs, "These offsets passed a clean lap, but some cores may have room left, so I'm trying to win back depth.")
	if r := s.refine; r != nil {
		var moves, checks []string
		for i, c := range s.cores {
			if slices.Contains(r.Cores, c.id) && i < len(r.Profile) {
				move := "yields"
				if slices.ContainsFunc(r.Checks, func(check journal.CheckState) bool {
					return (check.Regime == machine.R1 || check.Regime == machine.R2) && slices.Contains(check.Cores, c.id)
				}) {
					move = "goes deeper"
				}
				moves = append(moves, fmt.Sprintf("core %02d %s to %d", c.id, move, r.Profile[i]))
			}
		}
		if len(moves) > 0 {
			st.paragraphs = append(st.paragraphs, fmt.Sprintf("Round %d: %s. The whole proposed profile stays applied during the checks; yielded cores don't need their own checks.", r.Round, strings.Join(moves, ", ")))
		}
		done, total := 0, 0
		for _, c := range r.Checks {
			done += min(c.Passes, c.Needed)
			total += c.Needed
			checks = append(checks, fmt.Sprintf("%d %s runs on %s", c.Needed, regimeWords[c.Regime], coreList(c.Cores)))
		}
		if total > 0 {
			st.paragraphs = append(st.paragraphs, "The scheduled checks are "+strings.Join(checks, ", ")+".")
			st.paragraphs = append(st.paragraphs, fmt.Sprintf("%d of %d runs in this round have passed. If one fails, the round stops and that failure is handled first.", done, total))
		}
	}
	return st
}

func (s Snapshot) rerunStory() story {
	text := fmt.Sprintf("A failure just moved some offsets back. Now the failed test needs %d starts of %s.", s.starts, duration(s.startDuration))
	if s.trial != nil && s.trial.duration != s.startDuration {
		text = fmt.Sprintf("The initial repeats passed. Now I rerun the failed test once at its original length, %s, before the interrupted work continues.", duration(s.trial.duration))
	} else if s.rerunDuration != s.startDuration && s.rerunDuration > 0 {
		text += fmt.Sprintf(" Then it needs one start of %s, its original length.", duration(s.rerunDuration))
	}
	if s.trial == nil || s.trial.duration == s.startDuration {
		text += " Once those checks pass, the interrupted work continues."
	}
	return story{headline: s.headline(), tone: plainTone, paragraphs: []string{text}}
}

func (s Snapshot) lapStory(t *trial) story {
	g := s.guard
	if s.goal() {
		st := story{headline: "KEEPING WATCH  ∞", tone: goodTone, paragraphs: []string{
			"Every core has found its limit, and these offsets passed a full lap of every kind of test: light and heavy loads, load steps, partial load, both threads of a core, idle, and all cores at once.",
			"Passing tests can't prove a profile will never fail, so I keep running laps to catch rare failures. Stop me whenever you like. These offsets are the ones to carry into the BIOS.",
		}}
		st.paragraphs = append(st.paragraphs, "Right now: "+describeLoad(t, len(s.cores))+".")
		return st
	}
	st := story{headline: "TESTING TOGETHER", tone: plainTone}
	steps := 0
	if g != nil {
		steps = len(g.Steps)
	}
	coverage := "that covers every kind of load"
	if !s.guardQualifying {
		coverage = "from the configured schedule"
	}
	st.paragraphs = append(st.paragraphs, fmt.Sprintf(
		"Every core found its limit on its own. Now they all run at their offsets together, through a lap of %d steps %s.", steps, coverage))
	if why, ok := regimeExplained[t.regime]; ok {
		st.paragraphs = append(st.paragraphs, fmt.Sprintf("This step is %s: %s. It runs %s.", regimeWords[t.regime], why, describeLoad(t, len(s.cores))))
	}
	if s.guardQualifying {
		st.paragraphs = append(st.paragraphs, "The goal is a clean lap: every step passes with no failure, on offsets that can't go any deeper.")
	} else {
		st.paragraphs = append(st.paragraphs, s.missingCoverage())
	}
	return st
}

// describeLoad says where a test's load runs, in plain words.
func describeLoad(t *trial, total int) string {
	switch {
	case t.regime == machine.R6:
		return "with every core mostly idle and short bursts on one core at a time"
	case len(t.cores) == 1:
		return fmt.Sprintf("on one core at a time, now core %02d, while the others sit idle at their offsets", t.cores[0])
	}
	return "on " + coresText(t.cores, total) + " at once"
}

func (s Snapshot) nowLine(t *trial, now time.Time) *nowLine {
	n := &nowLine{detail: regimeWords[t.regime] + " on " + coresText(t.cores, len(s.cores)), backend: t.workload}
	if t.condition == machine.Isolated && t.offset != nil {
		n.detail += fmt.Sprintf(" alone at %d", *t.offset)
	}
	switch {
	case t.condition == machine.Masked && s.hunt != nil && s.hunt.mask != nil:
		m := s.hunt.mask
		n.what = fmt.Sprintf("hunt %d, test %d, run %d of %d", s.hunt.id, m.id, min(m.passes+1, m.needed), m.needed)
	case t.round != 0:
		n.what = fmt.Sprintf("deepening round %d", t.round)
	case t.condition == machine.Isolated:
		n.what = "search step"
		if len(t.cores) > 0 && s.core(t.cores[0]).checking {
			n.what = "confirming the limit"
		}
	case s.guard != nil && len(s.guard.Steps) > 0:
		n.what = fmt.Sprintf("lap %d, step %d of %d", s.guard.Rotation, currentStep(s.guard), len(s.guard.Steps))
	}
	if t.rerun {
		n.what = "rerun after a fix"
	}
	if !t.hasStarted {
		n.what = "preparing " + n.what
		n.detail = "planned: " + n.detail
	}
	if t.hasStarted && t.duration > 0 {
		elapsed := min(max(now.Sub(t.started), 0), t.duration)
		n.progress, n.timed, n.left = float64(elapsed)/float64(t.duration), true, t.duration-elapsed
	}
	return n
}

func (s Snapshot) deadEndStory() story {
	d := s.deadEnd
	why := map[journal.DeadEndCondition]string{
		journal.DeadEndFailureAtZero: "A core failed even at offset 0, so the problem isn't Curve Optimizer. Check the rest of the system before tuning again.",
		journal.DeadEndSMU:           "The CPU didn't take an offset the way I wrote it, so I can't trust what is applied. Nothing else will be written.",
		journal.DeadEndNoEvidence:    "I couldn't obtain usable failure evidence, so I can't continue testing safely. Check the recorded details before tuning again.",
		journal.DeadEndBootLoop:      "The machine crashed three times in a row before I applied any offsets: something else is crashing it.",
		journal.DeadEndContainment:   "I couldn't confirm that the stress programs were contained and cleaned up safely, so tuning can't continue.",
		journal.DeadEndPreflight:     "A check before tuning failed: this isn't the environment I tune in.",
		journal.DeadEndDefect:        "An earlier build may have moved offsets deeper than proven. Answer the reset question at a terminal before tuning continues.",
		journal.DeadEndThermalTrip:   "The CPU shut down from heat. Check the cooling before tuning again.",
	}[d.condition]
	if why == "" {
		why = "I can't make progress from here."
	}
	return story{headline: "I STOPPED", tone: badTone, paragraphs: []string{why, "Details: " + d.detail}}
}

// comingUp lists what follows the current work, as far as the journal tells.
func (s Snapshot) comingUp() []string {
	switch {
	case !s.session || s.deadEnd != nil || s.stopped != nil:
		return nil
	case s.trial != nil && s.trial.rerun:
		if s.trial.duration != s.startDuration {
			return []string{fmt.Sprintf("If this one original-length start of %s passes, the work the failure interrupted continues.", duration(s.trial.duration))}
		}
		line := fmt.Sprintf("The failed test needs %d passing starts of %s", s.starts, duration(s.startDuration))
		if s.rerunDuration != s.startDuration && s.rerunDuration > 0 {
			line += fmt.Sprintf(", then one start of %s at the original length", duration(s.rerunDuration))
		}
		return []string{line + ". Once those checks pass, the work the failure interrupted continues."}
	case s.phase == journal.PhaseSearch:
		return s.searchNext()
	case s.phase == journal.PhaseHunt:
		return []string{
			fmt.Sprintf("When the hunt ends, I record its result and move the offsets back past it. Then the failed test needs %d starts of %s, followed by one at its original length if that differs.", s.starts, duration(s.startDuration)),
			"Then the work the failure interrupted continues.",
		}
	case s.phase == journal.PhaseRefine:
		return []string{"If the round passes, the deeper offsets need a new clean lap. If not, I handle the failure first."}
	}
	return s.lapNext()
}

func (s Snapshot) searchNext() []string {
	current := -1
	if s.trial != nil && len(s.trial.cores) == 1 {
		current = s.trial.cores[0]
	}
	start := slices.Index(s.order, current) + 1
	var next []int
	for k := range s.order {
		id := s.order[(start+k)%len(s.order)]
		if id != current && s.core(id).phase == journal.PhaseSearch {
			next = append(next, id)
		}
	}
	lines := []string{}
	if len(next) > 0 {
		ids := make([]string, len(next))
		for i, id := range next {
			ids[i] = fmt.Sprintf("%02d", id)
		}
		lines = append(lines, "Next in line, taking turns so each core cools down between its own tests: "+strings.Join(ids, ", ")+".")
	}
	if !s.guardQualifying {
		return append(lines, "When every core has its limit, they run together through the configured test schedule.", s.missingCoverage())
	}
	return append(lines, "When every core has its limit, they all run together through laps of every kind of test.")
}

func (s Snapshot) lapNext() []string {
	g := s.guard
	if g == nil || len(g.Steps) == 0 {
		return nil
	}
	rest := g.Steps[currentStep(g):]
	var parts []string
	for i := 0; i < len(rest); {
		j := i
		for j+1 < len(rest) && rest[j+1] == rest[i] {
			j++
		}
		w := regimeWords[rest[i]]
		if j > i {
			w += fmt.Sprintf(" x%d", j-i+1)
		}
		parts = append(parts, w)
		i = j + 1
	}
	var lines []string
	if len(parts) > 0 {
		lines = append(lines, "Rest of this lap: "+strings.Join(parts, ", ")+".")
	}
	if !s.guardQualifying {
		return append(lines, s.missingCoverage(), "Then another lap of the configured schedule, until you stop me.")
	}
	if s.goal() {
		return append(lines, "Then another lap, until you stop me.")
	}
	return append(lines, "If the lap finishes clean on offsets that can't go deeper, that's the goal. After that I keep checking.")
}

func (s Snapshot) missingCoverage() string {
	if s.goal() {
		text := "This schedule doesn't cover every kind of test. Its future laps don't add qualifying clean-lap credit, but the goal is already reached."
		if len(s.guardMissing) > 0 {
			text += " Missing: " + strings.Join(s.guardMissing, "; ") + "."
		}
		return text
	}
	text := "This schedule doesn't cover every kind of test, so its laps can't qualify for the clean-lap goal."
	if len(s.guardMissing) > 0 {
		text += " Missing: " + strings.Join(s.guardMissing, ", ") + "."
	}
	return text
}

type station struct {
	label  string
	state  stationState
	sub    []string
	fill   float64
	paused bool
}

type stationState int

const (
	upcoming stationState = iota
	current
	reached
	target
	endless
)

func (s Snapshot) stations() []station {
	left := s.searching()
	total := len(s.cores)
	g := s.guard
	lap, step := "", ""
	fill := 0.0
	if g != nil && len(g.Steps) > 0 {
		lap, step = fmt.Sprintf("lap %d", g.Rotation), fmt.Sprintf("step %d of %d", currentStep(g), len(g.Steps))
		fill = float64(g.StepsDone) / float64(len(g.Steps))
	}
	find := station{label: "Find limits", state: reached, sub: []string{fmt.Sprintf("%d/%d cores", total-left, total)}}
	together := station{label: "Test together", state: upcoming, sub: []string{"all together"}}
	deeper := station{label: "Go deeper", state: upcoming, sub: []string{"after a", "clean lap"}}
	clean := station{label: "Clean lap", state: target, sub: []string{"the goal"}}
	keep := station{label: "Keep checking", state: endless, sub: []string{"until stopped"}}
	if !s.guardQualifying {
		clean.state, clean.sub = upcoming, []string{"not covered", "by schedule"}
		deeper.sub = []string{"needs a full", "clean lap"}
	}
	switch {
	case left > 0:
		find.state, find.fill = current, float64(total-left)/float64(max(total, 1))
	case s.goal():
		together.state, together.sub = reached, []string{plural(g.Rotation, "lap")}
		deeper.state, deeper.sub = reached, []string{"no room left"}
		clean.state, clean.sub = reached, []string{"reached"}
		keep.state, keep.fill, keep.sub = current, fill, []string{lap, plural(g.CleanRotations, "clean lap")}
	case s.refine != nil:
		together.state, together.sub = reached, []string{"lap was clean"}
		deeper.state, deeper.sub = current, []string{fmt.Sprintf("round %d", s.refine.Round)}
		deeper.paused = s.phase == journal.PhaseHunt
	default:
		together.state, together.fill, together.sub = current, fill, []string{lap, step}
		together.paused = s.phase == journal.PhaseHunt
	}
	for _, st := range []*station{&together, &deeper} {
		if st.paused {
			st.sub = append(st.sub[:1], "paused")
		}
	}
	return []station{find, together, deeper, clean, keep}
}

func capital(s string) string {
	if s == "" {
		return s
	}
	return strings.ToUpper(s[:1]) + s[1:]
}

func are(cores []int) string {
	if len(cores) == 1 {
		return "is"
	}
	return "are"
}

func article(noun string) string {
	if noun != "" && strings.ContainsRune("aeiou", rune(noun[0])) {
		return "an " + noun
	}
	return "a " + noun
}
