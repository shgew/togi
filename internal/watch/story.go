package watch

import (
	"fmt"
	"strings"
	"time"

	"charm.land/lipgloss/v2"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

type story struct {
	headline   string
	paragraphs []string
	tone       tone
}

func (s Snapshot) story(now time.Time) story {
	switch {
	case s.problem != nil:
		return story{"I CAN'T READ THE JOURNAL", []string{vtText(s.problem.Error()), "I'll retry when the journal changes. Tuning itself is not affected by this screen."}, badTone}
	case !s.session:
		return story{"NO SESSION YET", []string{"Nothing has been recorded yet. Start with togi run, or boot the togi entry, and I'll tell you what I'm doing here."}, plainTone}
	case s.deadEnd != nil:
		return s.deadEndStory()
	case s.stopped != nil:
		text := "I've stopped. Everything I learned is in the journal; togi run picks up where I left off."
		if s.stopped.saved {
			text = "The rows show the saved profile, not applied now. I restored safer offsets before stopping."
		}
		return story{"STOPPED", []string{text, stopWords(s.stopped.reason)}, plainTone}
	case s.recover != nil:
		return story{"RECOVERED FROM A CRASH", []string{"The machine crashed and rebooted. No trial has started since I recovered on this boot.", s.nextLine()}, warnTone}
	case s.trial == nil:
		return story{"BETWEEN TRIALS", []string{"I'm between trials. The journal keeps the last result and the next trial when it is known.", s.nextLine()}, plainTone}
	}
	t := s.trial
	if !t.hasStarted {
		return story{"PREPARING", []string{"I'm waiting for the workload to start. Its intent is recorded, but the trial has not started yet.", "Planned: " + trialDescription(*t) + " for " + shortDuration(t.duration) + "."}, plainTone}
	}
	if t.recordOnly {
		return story{"RECORD ONLY", []string{"This partial load keeps a record, but its result changes no decision and moves no offset.", "A pass or a failure completes this part; it supplies no full-cycle coverage."}, plainTone}
	}
	if s.hunt != nil {
		h := s.hunt
		if t.probe != nil {
			return story{fmt.Sprintf("HUNT %d", h.id), []string{"The group was kept together. I probe each member: how shallow must it go for the rest to pass?", fmt.Sprintf("Core %02d is at %d; held members keep their recorded failing offsets.", t.probe.Core, t.probe.Offset)}, huntTone}
		}
		cause := "A trial failed and named no core."
		if h.cause.signal == machine.Crash {
			cause = "The machine crashed during " + string(h.cause.trial.regime) + " " + loadWords(h.cause.trial.regime) + " and rebooted. No core was named."
		}
		return story{fmt.Sprintf("HUNT %d", h.id), []string{cause, "I rerun that load with " + coreIDs(h.candidates) + " split between failing and parked offsets."}, huntTone}
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
		return story{"SOLO LIMITS", []string{text, "One core at a time with the rest at 0, so any failure belongs to the loaded core."}, plainTone}
	}
	if t.rerun {
		return story{"RERUN", []string{"The failed load runs again after the offsets changed. This trial checks the new profile.", fmt.Sprintf("%d passed so far; this is trial %d of %d of this requirement.", t.passed, t.index, t.of)}, plainTone}
	}
	if t.round > 0 || s.phase == journal.PhaseDeepening {
		return story{fmt.Sprintf("DEEPEN · ROUND %d", t.round), []string{"These offsets passed a full cycle. I check the deepening plan's proposed profile together.", "The checks below distinguish cores that go deeper from members that yield shallower."}, plainTone}
	}
	name := fmt.Sprintf("CYCLE %d", t.cycle)
	if t.regime == machine.R6 {
		until := t.started.Add(t.duration)
		if now.Before(until) {
			return story{name, []string{fmt.Sprintf("Step %d leaves every core idle for %s, with short wake-ups, to test idle and boost states.", t.step, shortDuration(t.duration)), "This screen holds still until " + until.Format("15:04") + ", clock included, so drawing it cannot wake the cores."}, plainTone}
		}
		return story{name, []string{fmt.Sprintf("The idle trial was planned to end at %s. I'm waiting for its result in the journal.", until.Format("15:04"))}, plainTone}
	}
	text := fmt.Sprintf("Step %d is %s %s with %s.", t.step, t.regime, loadWords(t.regime), workloadDisplay(t.workload))
	where := "Every core runs at its profile offset. The cycle keeps testing until stopped."
	if t.parts > 0 {
		where = fmt.Sprintf("Part %d of %d loads %s for %s.", t.part, t.parts, coreIDs(t.cores), shortDuration(t.duration))
	}
	if s.cleanCycles > 0 {
		where = fmt.Sprintf("%d clean cycles count for this profile. Passing trials cannot prove it will never fail.", s.cleanCycles)
	}
	return story{name, []string{text, where}, plainTone}
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
	return story{"DEAD END", []string{why, "Details: " + vtText(d.detail)}, badTone}
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

func trialWhere(t trialView) string {
	where := "on " + coreIDs(t.cores)
	if t.condition == machine.Alone {
		where = fmt.Sprintf("on core %02d alone at %d", t.core, t.offset)
	} else if len(t.parked) > 0 {
		where += " · " + coreIDs(t.parked) + " parked"
		if len(t.idle) > 0 {
			where += ", idle " + coreIDs(t.idle)
		}
	}
	return where
}

func trialDescription(t trialView) string {
	return string(t.regime) + " " + loadWords(t.regime) + " " + trialWhere(t)
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
		if t.parts > 0 {
			text += fmt.Sprintf(" · PART %d", t.part)
		}
	}
	if t.recordOnly {
		text += " · RECORD ONLY"
	}
	return tone.Render(text)
}

func lastDescription(t trialEnd) string {
	out := strings.ToUpper(string(t.outcome)) + " · " + string(t.regime) + " on " + coreIDs(t.cores) + " · " + clock(t.duration)
	if t.signal != "" {
		out += " · " + signalWords[t.signal]
	}
	if t.core != nil {
		out += fmt.Sprintf(" · core %02d named", *t.core)
	}
	return out
}

func (s Snapshot) lastLines() []string {
	if s.last == nil {
		return nil
	}
	t := s.last
	style := green
	switch t.outcome {
	case journal.OutcomeFailure:
		style = red
	case journal.OutcomeInconclusive:
		style = amber
	case journal.OutcomePass:
	}
	line := grey.Render("last trial      ") + style.Render(strings.ToUpper(string(t.outcome))) + grey.Render("  "+clock(t.duration)+" on "+coreIDs(t.cores))
	if t.tctlMaxC != nil {
		line += grey.Render(fmt.Sprintf(" · Tctl max %d C", *t.tctlMaxC))
	}
	if t.voltageV != nil {
		line += grey.Render(fmt.Sprintf(" · %.2f V median", *t.voltageV))
	}
	return []string{line}
}

func (s Snapshot) nextLine() string {
	if s.next == nil {
		return "next: deciding"
	}
	return "next: " + forecastTrial(*s.next)
}

func forecastTrial(t tuner.Trial) string {
	where := coreIDs(t.Cores)
	if len(t.Cores) == 0 {
		where = fmt.Sprintf("core %02d at %d", t.Core, t.Offset)
	}
	text := string(t.Regime) + " " + loadWords(t.Regime) + " on " + where
	if t.Workload != "" {
		text += " with " + workloadDisplayID(t.Workload)
	}
	switch {
	case t.Hunt > 0:
		text = fmt.Sprintf("hunt %d group %d: %s", t.Hunt, t.Group, text)
	case t.Cycle > 0:
		text = fmt.Sprintf("cycle %d step %d: %s", t.Cycle, t.Step, text)
	case t.Round > 0:
		text = fmt.Sprintf("deepening round %d: %s", t.Round, text)
	}
	if t.Rerun {
		text = "rerun " + text
	}
	if t.RecordOnly {
		text += " · record only"
	}
	return text
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

func memberCoreIDs(members []journal.CombinationMember) string {
	ids := make([]int, len(members))
	for i, member := range members {
		ids[i] = member.Core
	}
	return coreIDs(ids)
}

func memberSummary(members []journal.CombinationMember) string {
	if len(members) <= 2 {
		return compactMembers(members)
	}
	return memberCoreIDs(members) + " at recorded offsets"
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
