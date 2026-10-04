package watch

import (
	"fmt"
	"slices"
	"strings"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

var regimeWords = map[machine.Regime]string{
	machine.R1: "light load",
	machine.R2: "heavy vector load",
	machine.R3: "load steps",
	machine.R4: "partial load",
	machine.R5: "both threads",
	machine.R6: "idle with short bursts",
	machine.R7: "all-core load",
}

var signalWords = map[machine.Signal]string{
	machine.ComputationError: "wrong result",
	machine.UnexpectedExit:   "the stress program quit",
	machine.Stall:            "the stress program stalled",
	machine.CorrectedMCE:     "corrected hardware error",
	machine.UncorrectedMCE:   "uncorrected hardware error",
	machine.Crash:            "crash",
}

// signalPlurals name repeated failures of one kind, as a merged line counts them.
var signalPlurals = map[machine.Signal]string{
	machine.ComputationError: "computation errors",
	machine.UnexpectedExit:   "unexpected exits",
	machine.Stall:            "stalls",
	machine.CorrectedMCE:     "corrected hardware errors",
	machine.UncorrectedMCE:   "uncorrected hardware errors",
	machine.Crash:            "crashes",
}

// The tags of what happened: one short word per kind of event, in a column of their own, so the eye finds a crash
// without reading the sentence beside it.
const (
	tagStart   = "start"
	tagPass    = "pass"
	tagFail    = "fail"
	tagCrash   = "crash"
	tagUnclear = "unclear"
	tagCause   = "fail"
	tagSkip    = "skip"
	tagReset   = "reset"
	tagSearch  = "step"
	tagLimit   = "limit"
	tagDeeper  = "room"
	tagBackoff = "backoff"
	tagYield   = "room"
	tagCycle   = "cycle"
	tagHunt    = "hunt"
	tagCombo   = "combo"
	tagRound   = "round"
	tagStop    = "stop"
	tagMCE     = "mce"
	tagNote    = "note"
	tagWidth   = 7
	tagRecord  = "record"
	tagGroup   = "group"
	tagProbe   = "probe"
	tagConfirm = "confirm"
	tagResume  = "resume"
)

// entryKind says how an entry builds its sentence from what merged into it.
type entryKind int

const (
	plainEntry   entryKind = iota
	trialsEntry            // trials of one requirement or part: subject, then how many passed or failed
	limitsEntry            // solo limits found at one offset
	groupsEntry            // hunt groups answered by carried trials
	sessionEntry           // the session start, with the solo limits carried into it
)

func signalText(sig machine.Signal) string {
	if w, ok := signalWords[sig]; ok {
		return w
	}
	return vtText(strings.ReplaceAll(string(sig), "_", " "))
}

func signalPlural(sig machine.Signal, n int) string {
	if n == 1 {
		return signalText(sig)
	}
	if w, ok := signalPlurals[sig]; ok {
		return w
	}
	return signalText(sig) + "s"
}

// describe turns one journal event into a line of what happened, or changes the line an earlier event started.
func (p *projector) describe(e journal.Event) (entry, bool) {
	line := entry{at: e.Time, tone: plainTone}
	switch d := e.Data.(type) {
	case *journal.SessionStart:
		line.kind, line.tag, line.of = sessionEntry, tagStart, len(d.Cores)
	case *journal.SessionCarried:
		p.carrying = true
		return entry{}, false
	case *journal.ConfigLoaded:
		if p.configs < 2 {
			return entry{}, false
		}
		line.tag, line.text = tagResume, "session resumed"
	case *journal.TrialIntent:
		p.carrying = false
		p.stepWorkload(d)
		return entry{}, false
	case *journal.TrialEnd:
		return p.trialEnd(line, d, e.Cause)
	case *journal.Failure:
		if d.KnownFailure != 0 {
			return p.knownFailure(line, d), true
		}
		line.tag, line.text, line.tone = failureText(d)
	case *journal.CrashDetected:
		if !d.Stray && !d.Inconclusive {
			line.reboot = e.Seq
		}
		switch {
		case d.Stray:
			line.tag, line.text, line.tone = tagCrash, "before any offsets were applied, rebooted", warnTone
		case d.Inconclusive:
			line.tag, line.text = tagReset, "power loss or reset, which says nothing about the offsets"
		case d.InFlight == nil:
			line.tag, line.text, line.tone = tagCrash, "idle with the offsets applied, rebooted", badTone
		default:
			line.tag, line.text, line.tone = tagCrash, "with the offsets applied, rebooted", badTone
		}
		if n := len(p.s.history); n > 0 && p.s.history[n-1].tag == tagResume {
			p.s.history = p.s.history[:n-1]
		}
	case *journal.TunerDecision:
		line.tag, line.text, line.tone = decisionText(d)
		if d.Decision == journal.Backoff && p.sources[e.Seq] != 0 {
			line.text += fmt.Sprintf(" · clear of C%d", p.sources[e.Seq])
		}
	case *journal.CorePhase:
		return p.corePhase(line, d)
	case *journal.CheckingCycle:
		line.tag, line.text, line.tone = cycleText(d)
		if d.Event == journal.CycleStart {
			p.cycleSteps[d.Cycle] = d.Steps
		}
	case *journal.HuntStart:
		return p.huntStart(line, d)
	case *journal.HuntGroup:
		return p.huntGroup(line, d)
	case *journal.CheckingStep:
		line.tag, line.text = tagSearch, fmt.Sprintf("cycle %d step %d", d.Cycle, d.Step)
		if regimes := p.cycleSteps[d.Cycle]; d.Step >= 1 && d.Step <= len(regimes) {
			line.text += fmt.Sprintf(": %s %s", regimes[d.Step-1], kindWords(regimes[d.Step-1]))
		}
		line.key = fmt.Sprintf("step %d %d", d.Cycle, d.Step)
	case *journal.HuntEnd:
		line.tag, line.text, line.tone = p.huntEndText(d, e.Time)
	case *journal.HuntSkipped:
		line.tag, line.text = tagHunt, "not needed, these offsets already reach a known failure"
	case *journal.Combination:
		line.tag, line.text, line.tone = tagCombo, combinationText(d), comboTone
	case *journal.DeepeningRound:
		line.tag, line.text, line.tone = p.roundText(d)
	case *journal.DeadEnd:
		line.tag, line.text, line.tone = tagStop, "dead end: "+vtText(d.Detail), badTone
	case *journal.Shutdown:
		line.tag, line.text = tagStop, "stopped"
	case *journal.MCE:
		if !d.BetweenTrials {
			return entry{}, false
		}
		kind := "uncorrected"
		if d.Corrected {
			kind = "corrected"
		}
		line.tag, line.text, line.tone = tagMCE, fmt.Sprintf("core %02d %s error between trials (recorded only)", d.Core, kind), warnTone
	case *journal.CommandReset:
		line.tag, line.text = tagReset, "requested from the command line"
	case *journal.DefectFound, *journal.SessionWarning, *journal.BackendRetry:
		line.tag, line.text, line.tone = tagNote, vtText(e.Msg), warnTone
	default:
		return entry{}, false
	}
	return line, line.text != "" || line.kind != plainEntry
}

// knownFailure is the skip of a trial that already failed at these offsets; the hunt it starts completes the line.
func (p *projector) knownFailure(line entry, d *journal.Failure) entry {
	verb := "failed"
	if d.Signal == machine.Crash {
		verb = "crashed"
	}
	line.tag, line.tone = tagSkip, plainTone
	line.text = fmt.Sprintf("%s %s at these offsets already %s", d.Regime, kindWords(d.Regime), verb)
	if p.s.carried[d.KnownFailure] {
		line.text += " in a carried trial"
	}
	line.key = "known failure"
	return line
}

// corePhase counts solo limits carried into the session on its start line, and merges solo limits found together.
func (p *projector) corePhase(line entry, d *journal.CorePhase) (entry, bool) {
	if p.carrying && d.From == journal.PhaseSearch && d.To != journal.PhaseSearch {
		for i := len(p.s.history) - 1; i >= 0; i-- {
			if p.s.history[i].kind == sessionEntry {
				p.s.history[i].count++
				break
			}
		}
		return entry{}, false
	}
	line.tag, line.text, line.tone = phaseText(d)
	if line.tag == tagLimit {
		line.kind, line.cores, line.offset, line.key = limitsEntry, []int{d.Core}, d.Offset, "limit"
	}
	return line, line.text != ""
}

// huntStart completes a skip line with the hunt it starts, or begins the hunt's own line.
func (p *projector) huntStart(line entry, d *journal.HuntStart) (entry, bool) {
	p.huntStarts[d.Hunt] = huntStartView{at: line.at, candidates: d.Candidates}
	if n := len(p.s.history); n > 0 && p.s.history[n-1].key == "known failure" && p.s.history[n-1].hunt == 0 {
		last := &p.s.history[n-1]
		last.hunt = d.Hunt
		last.text = strings.Replace(last.text, " at these", " on "+coreIDs(d.Cores)+" at these", 1) + fmt.Sprintf(" → hunt %d", d.Hunt)
		return entry{}, false
	}
	line.tag, line.text, line.tone, line.hunt = tagHunt, fmt.Sprintf("#%d started · candidates %s", d.Hunt, coreIDs(d.Candidates)), warnTone, d.Hunt
	line.key = "hunt start"
	return line, true
}

// combinationText names a combination: its members' offsets when a hunt showed them failing together, its cores when
// it is an unresolved fallback.
func combinationText(d *journal.Combination) string {
	if d.Fallback {
		return fmt.Sprintf("C%d over %s · hunt %d unresolved", d.Combination, memberCoreIDs(d.Members), d.Hunt)
	}
	parts := make([]string, len(d.Members))
	for i, m := range d.Members {
		parts[i] = fmt.Sprintf("%02d %d", m.Core, m.Offset)
	}
	return fmt.Sprintf("C%d: %s", d.Combination, strings.Join(parts, "  "))
}

// stepWorkload names the workload of a checking step on its line once its first trial says which it is.
func (p *projector) stepWorkload(in *journal.TrialIntent) {
	if in.Cycle == 0 || in.Step == 0 {
		return
	}
	key := fmt.Sprintf("step %d %d", in.Cycle, in.Step)
	for i := len(p.s.history) - 1; i >= 0; i-- {
		h := &p.s.history[i]
		if h.key != key {
			continue
		}
		if !strings.Contains(h.text, ", ") {
			h.text += ", " + workloadDisplayID(in.Workload)
		}
		return
	}
}

func (p *projector) huntGroup(line entry, d *journal.HuntGroup) (entry, bool) {
	p.groups[[2]int{d.Hunt, d.Group}] = d
	start := p.huntStarts[d.Hunt]
	if d.Probe == nil && !start.named && len(d.Cores) > 0 {
		start.named = true
		p.huntStarts[d.Hunt] = start
		for i := len(p.s.history) - 1; i >= 0; i-- {
			h := &p.s.history[i]
			if h.key == "hunt start" && h.hunt == d.Hunt {
				h.text = fmt.Sprintf("#%d started · part 1: %s", d.Hunt, partLayout(d.Cores, start.candidates))
				break
			}
		}
	}
	if d.Inferred == "" && !d.Skipped {
		start.live++
		p.huntStarts[d.Hunt] = start
		return entry{}, false
	}
	line.hunt, line.firstGroup, line.lastGroup = d.Hunt, d.Group, d.Group
	switch {
	case d.Skipped:
		line.tag, line.text = tagSkip, fmt.Sprintf("hunt %d group %d skipped: %s", d.Hunt, d.Group, vtText(d.Reason))
	case d.Probe != nil:
		verb := "passed"
		if d.Inferred != "pass" {
			verb = "failed"
		}
		line.tag, line.tone = tagProbe, huntTone
		line.text = fmt.Sprintf("hunt %d group %d · core %02d at %d · %s in a carried trial", d.Hunt, d.Group, d.Probe.Core, d.Probe.Offset, verb)
	default:
		line.kind, line.tag, line.tone = groupsEntry, tagGroup, huntTone
		line.cores, line.text = slices.Clone(d.Cores), d.Inferred
		line.key = fmt.Sprintf("carried %d %s", d.Hunt, d.Inferred)
	}
	return line, true
}

// partLayout says which candidates a hunt group runs at their failing offsets and which it parks.
func partLayout(cores, candidates []int) string {
	text := coreIDs(cores) + " at failing offsets"
	if rest := without(candidates, cores); len(rest) > 0 {
		text += ", " + coreIDs(rest) + " parked"
	}
	return text
}

// trialEnd tells how a trial ended; repeated outcomes of one requirement or part merge into one line.
func (p *projector) trialEnd(line entry, d *journal.TrialEnd, cause []int) (entry, bool) {
	in := p.intents[d.Trial]
	count := p.counts[d.Trial]
	if in == nil {
		line.text = "trial " + vtText(d.Trial)
	} else {
		if in.Condition == machine.Alone && d.Outcome == journal.OutcomePass {
			return entry{}, false
		}
		line.text = p.trialName(in)
	}
	line.first, line.of, line.runs = count.index, count.of, 1
	if in != nil && in.Hunt > 0 {
		line.hunt, line.firstGroup, line.lastGroup = in.Hunt, in.Group, in.Group
		line.probe = p.groups[[2]int{in.Hunt, in.Group}] != nil && p.groups[[2]int{in.Hunt, in.Group}].Probe != nil
	}
	record := in != nil && in.RecordOnly
	switch d.Outcome {
	case journal.OutcomePass:
		line.kind, line.tag, line.tone, line.peak = trialsEntry, tagPass, goodTone, d.TctlMaxC
		line.key = "pass " + line.text
		if record {
			line.tag, line.tone, line.tail = tagRecord, plainTone, " · record only"
			line.key = "record " + line.text
		}
	case journal.OutcomeFailure:
		if record {
			line.kind, line.tag, line.tone, line.signal = trialsEntry, tagRecord, plainTone, d.Signal
			line.key, line.tail = "record "+string(d.Signal)+" "+line.text, " · record only, moves nothing"
			if d.Signal == machine.Crash {
				line.tail = " · record only"
			}
		} else {
			named := ""
			switch {
			case d.Core != nil:
				named = fmt.Sprintf(" · core %02d named", *d.Core)
			case in != nil && in.Hunt == 0 && in.Condition != machine.Alone:
				named = " · no core named"
			}
			if d.Signal == machine.Crash {
				line.tag, line.tone = tagCrash, badTone
				line.text += " · rebooted"
				if late := p.lateness(d, in, count); late != "" {
					line.text += " · " + late
				}
			} else {
				line.tag, line.tone = tagFail, badTone
				line.text += " · " + signalText(d.Signal)
				line.alarm = signalText(d.Signal)
			}
			line.text += named
		}
		if d.Signal == machine.Crash {
			// Recovery appends the trial outcome after the reboot; only an exact cause links their history.
			for i := len(p.s.history) - 1; i >= 0; i-- {
				previous := &p.s.history[i]
				if previous.reboot != 0 && slices.Contains(cause, previous.reboot) {
					line.reboot, line.at = previous.reboot, previous.at
					*previous = line
					if i == len(p.s.history)-1 && i > 0 {
						merged := foldEntry(slices.Clone(p.s.history[:i]), line)
						if len(merged) == i {
							p.s.history = merged
						}
					}
					return entry{}, false
				}
			}
		}
	case journal.OutcomeInconclusive:
		reason := "it could not run"
		if d.Reason != "" {
			reason = vtText(d.Reason)
		}
		line.tag, line.text, line.tone = tagUnclear, line.text+" · "+reason, warnTone
	}
	return line, true
}

// lateness says how far into its planned length a crashed trial got, and which trial of its part or requirement it
// was: "late in trial 4 of 4".
func (p *projector) lateness(d *journal.TrialEnd, in *journal.TrialIntent, count trialCount) string {
	if d.LastSampleS == nil || in == nil || in.DurationS <= 0 {
		return ""
	}
	sample := time.Duration(*d.LastSampleS) * time.Second
	when := lateness(&trialEnd{lastSample: &sample, planned: time.Duration(in.DurationS) * time.Second})
	if count.of > 0 {
		return fmt.Sprintf("%s trial %d of %d", when, count.index, count.of)
	}
	return when + " the trial"
}

// trialName names a trial in history: its regime, the part of its step, and its cores; a hunt trial by its group.
func (p *projector) trialName(in *journal.TrialIntent) string {
	cores := trialCores(in)
	what := fmt.Sprintf("%s %s", in.Regime, kindWords(in.Regime))
	switch {
	case in.Condition == machine.Alone && in.Offset != nil:
		return fmt.Sprintf("core %02d · %s at %d", cores[0], what, *in.Offset)
	case in.Hunt > 0:
		text := fmt.Sprintf("hunt %d group %d", in.Hunt, in.Group)
		if g := p.groups[[2]int{in.Hunt, in.Group}]; g != nil {
			if g.Probe != nil {
				members := slices.Clone(g.Cores)
				for _, m := range g.Held {
					if !slices.Contains(members, m.Core) {
						members = append(members, m.Core)
					}
				}
				if !slices.Contains(members, g.Probe.Core) {
					members = append(members, g.Probe.Core)
				}
				return text + fmt.Sprintf(" · %s with core %02d at %d", coreIDs(members), g.Probe.Core, g.Probe.Offset)
			}
			return text + " · " + coreIDs(g.Cores) + " at failing offsets"
		}
		return text
	case in.Regime == machine.R7 && in.Cycle > 0:
		what = string(in.Regime) + " " + p.partName(in)
	}
	text := what + " " + onCores(cores)
	if len(cores) == 1 {
		// A profile lists offsets in the session's core order, which need not match core IDs.
		if i := slices.IndexFunc(p.st.Cores, func(c journal.CoreState) bool { return c.Core == cores[0] }); i >= 0 && i < len(in.Profile) {
			text += fmt.Sprintf(" at %d", in.Profile[i])
		}
	}
	if in.Rerun {
		text = "rerun of " + text
	}
	return text
}

// partName names the part of an R7 step a trial loads, from the CCDs of its cores.
func (p *projector) partName(in *journal.TrialIntent) string {
	var ccds []string
	for _, c := range p.st.Cores {
		if slices.Contains(in.Cores, c.Core) && !slices.Contains(ccds, fmt.Sprint(c.CCD)) {
			ccds = append(ccds, fmt.Sprint(c.CCD))
		}
	}
	if in.RecordOnly {
		return "partial CCD " + strings.Join(ccds, "+")
	}
	return "full CCD " + strings.Join(ccds, "+")
}

func failureText(d *journal.Failure) (string, string, tone) {
	switch {
	case d.Attribution == journal.Attributed && d.Core != nil && d.Offset != nil:
		return tagCause, fmt.Sprintf("core %02d fails at %d", *d.Core, *d.Offset), warnTone
	case d.Trial == "" && d.Signal != machine.Crash:
		return tagFail, "idle with the offsets applied, " + signalText(d.Signal), badTone
	}
	return "", "", plainTone
}

func decisionText(d *journal.TunerDecision) (string, string, tone) {
	switch d.Decision {
	case journal.StepDeeper:
		return tagSearch, fmt.Sprintf("core %02d passed %d, next %d", d.Core, d.FromOffset, d.ToOffset), plainTone
	case journal.CheckSoloLimit:
		return tagConfirm, fmt.Sprintf("core %02d confirming %d as its solo limit", d.Core, d.ToOffset), plainTone
	case journal.Deepen:
		return tagDeeper, fmt.Sprintf("core %02d %d → %d", d.Core, d.FromOffset, d.ToOffset), goodTone
	case journal.Yield:
		return tagYield, fmt.Sprintf("core %02d %d → %d so others go deeper", d.Core, d.FromOffset, d.ToOffset), plainTone
	case journal.Backoff:
	}
	return tagBackoff, fmt.Sprintf("core %02d %d → %d", d.Core, d.FromOffset, d.ToOffset), warnTone
}

// phaseText tells when a core finds its solo limit, has room again or starts its search over; the other changes show
// on its row.
func phaseText(d *journal.CorePhase) (string, string, tone) {
	switch {
	case d.To == journal.PhaseSearch && d.From != "":
		return tagSearch, fmt.Sprintf("core %02d starts over from %d", d.Core, d.Offset), warnTone
	case d.From == journal.PhaseSearch:
		return tagLimit, fmt.Sprintf("core %02d solo limit %d", d.Core, d.Offset), goodTone
	case d.From == journal.PhaseAtLimit && d.To == journal.PhaseHasRoom:
		return tagDeeper, fmt.Sprintf("core %02d has room again at %d", d.Core, d.Offset), plainTone
	}
	return "", "", plainTone
}

func cycleText(d *journal.CheckingCycle) (string, string, tone) {
	switch {
	case d.Event == journal.CycleStart:
		return tagCycle, fmt.Sprintf("cycle %d started · %s", d.Cycle, plural(len(d.Steps), "step")), plainTone
	case d.Passed && d.Full:
		return tagCycle, fmt.Sprintf("cycle %d passed, a full cycle of every kind of test", d.Cycle), goodTone
	case d.Passed:
		return tagCycle, fmt.Sprintf("cycle %d passed but missing %s", d.Cycle, vtText(strings.Join(d.Missing, ", "))), warnTone
	}
	return tagCycle, fmt.Sprintf("cycle %d ended early: %s", d.Cycle, vtText(d.Reason)), warnTone
}

func (p *projector) huntEndText(d *journal.HuntEnd, at time.Time) (string, string, tone) {
	start := p.huntStarts[d.Hunt]
	text := fmt.Sprintf("#%d done", d.Hunt)
	if d.Groups > 0 {
		text += " · " + plural(d.Groups, "group")
	}
	switch {
	case start.live == 0 && d.Groups > 0:
		text += ", all answered by carried trials"
	case !start.at.IsZero() && d.Groups > 0:
		text += " in " + age(at.Sub(start.at))
	}
	switch d.Result {
	case "culprit", "direct":
		return tagHunt, text + " · " + onlyCore(d.Cores) + " named", goodTone
	case "combination":
		text += " · " + coreIDs(d.Cores) + " kept together"
		if p.probes[d.Hunt] {
			text += ", probed"
		}
		return tagHunt, text, warnTone
	case "fallback":
		return tagHunt, text + " · " + coreIDs(d.Cores) + " unresolved", warnTone
	case "cancelled":
		return tagHunt, fmt.Sprintf("#%d cancelled", d.Hunt), plainTone
	}
	return tagHunt, fmt.Sprintf("#%d ended: %s", d.Hunt, vtText(d.Reason)), plainTone
}

func onlyCore(cores []int) string {
	if len(cores) == 1 {
		return fmt.Sprintf("core %02d", cores[0])
	}
	return "cores " + coreIDs(cores)
}

func (p *projector) roundText(d *journal.DeepeningRound) (string, string, tone) {
	switch {
	case d.Event == journal.CycleStart:
		var moves []string
		for i, c := range p.st.Cores {
			if !slices.Contains(d.Cores, c.Core) || i >= len(d.Profile) {
				continue
			}
			move := "goes deeper"
			if d.Profile[i] > p.tuned[c.Core] {
				move = "yields"
			}
			moves = append(moves, fmt.Sprintf("core %02d %s to %d", c.Core, move, d.Profile[i]))
		}
		return tagRound, fmt.Sprintf("#%d: %s", d.Round, strings.Join(moves, ", ")), plainTone
	case d.Passed:
		return tagRound, fmt.Sprintf("#%d passed, the proposed offsets held", d.Round), goodTone
	}
	return tagRound, fmt.Sprintf("#%d stopped: %s", d.Round, vtText(d.Reason)), warnTone
}

func coresText(cores []int, total int) string {
	switch {
	case len(cores) == 1:
		return fmt.Sprintf("core %02d", cores[0])
	case len(cores) == total && total > 1:
		return fmt.Sprintf("all %d cores", total)
	}
	return coreList(cores)
}

// coreList names cores compactly: a run of three or more consecutive cores becomes a range.
func coreList(cores []int) string {
	if len(cores) == 0 {
		return "no cores"
	}
	sorted := slices.Sorted(slices.Values(cores))
	var parts []string
	for i := 0; i < len(sorted); {
		j := i
		for j+1 < len(sorted) && sorted[j+1] == sorted[j]+1 {
			j++
		}
		if j-i >= 2 {
			parts = append(parts, fmt.Sprintf("%02d-%02d", sorted[i], sorted[j]))
		} else {
			for k := i; k <= j; k++ {
				parts = append(parts, fmt.Sprintf("%02d", sorted[k]))
			}
		}
		i = j + 1
	}
	if len(sorted) == 1 {
		return "core " + parts[0]
	}
	return "cores " + strings.Join(parts, ", ")
}

// foldEntry appends a line of what happened, merging it into the line before when both carry the same key.
func foldEntry(history []entry, line entry) []entry {
	n := len(history)
	if n == 0 || line.key == "" || history[n-1].key != line.key {
		return append(history, line)
	}
	last := &history[n-1]
	switch line.kind {
	case limitsEntry:
		if last.offset != line.offset {
			return append(history, line)
		}
		last.cores = append(last.cores, line.cores...)
	case groupsEntry:
		if last.lastGroup+1 != line.firstGroup {
			return append(history, line)
		}
		last.lastGroup = line.lastGroup
		for _, c := range line.cores {
			if !slices.Contains(last.cores, c) {
				last.cores = append(last.cores, c)
			}
		}
	case trialsEntry:
		last.runs += line.runs
		last.of = max(last.of, line.of)
		if line.peak != nil && (last.peak == nil || *line.peak > *last.peak) {
			last.peak = line.peak
		}
	case plainEntry, sessionEntry:
		return append(history, line)
	}
	last.at = line.at
	return history
}

// mergeProbePasses folds adjacent member probes that each passed every trial they needed into one line.
func mergeProbePasses(history []entry) []entry {
	out := history[:0]
	for _, e := range history {
		complete := e.probe && e.tag == tagPass && e.of > 0 && e.runs == e.of && e.first == 1
		if n := len(out); n > 0 && complete {
			last := &out[n-1]
			if last.probe && last.tag == tagPass && last.hunt == e.hunt && last.lastGroup+1 == e.firstGroup && last.of == e.of && last.runs == last.of && last.first == 1 {
				last.at, last.lastGroup = e.at, e.lastGroup
				if e.peak != nil && (last.peak == nil || *e.peak > *last.peak) {
					last.peak = e.peak
				}
				continue
			}
		}
		out = append(out, e)
	}
	return out
}

// sentence is the line of what happened as plain text.
func (e entry) sentence() string {
	before, alarm, after := e.sentenceParts()
	return before + alarm + after
}

// sentenceParts splits the sentence around the words drawn as an alarm, such as the errors of a record-only part.
func (e entry) sentenceParts() (string, string, string) {
	switch e.kind {
	case sessionEntry:
		if e.count > 0 {
			return fmt.Sprintf("session started · %s carried from the last session", plural(e.count, "solo limit")), "", ""
		}
		return fmt.Sprintf("session started on %d cores", e.of), "", ""
	case limitsEntry:
		text := fmt.Sprintf("core %02d solo limit %d", e.cores[0], e.offset)
		if len(e.cores) > 1 {
			text = fmt.Sprintf("cores %s solo limit %d", commaIDs(slices.Sorted(slices.Values(e.cores))), e.offset)
		}
		if e.offset == -50 {
			text += ", the deepest offset"
		}
		return text, "", ""
	case groupsEntry:
		groups := fmt.Sprintf("group %d", e.firstGroup)
		if e.lastGroup > e.firstGroup {
			groups = fmt.Sprintf("groups %d-%d", e.firstGroup, e.lastGroup)
		}
		what := coreIDs(e.cores)
		verdict := "passed in a carried trial"
		if e.lastGroup > e.firstGroup {
			what = "parts of " + what
			verdict = "all passed in carried trials"
		}
		if e.text != "pass" {
			verdict = "failed in a carried trial"
		}
		return fmt.Sprintf("hunt %d %s · %s · %s", e.hunt, groups, what, verdict), "", ""
	case trialsEntry:
		if e.signal == machine.Crash {
			return e.text + " · ", plural(e.runs, "crash"), e.tail
		}
		if e.signal != "" {
			return e.text + " · ", signalPlural(e.signal, e.runs), " in " + e.tally() + e.tail
		}
		if e.probe && e.lastGroup > e.firstGroup {
			return fmt.Sprintf("hunt %d groups %d-%d · member probes · each %s passed", e.hunt, e.firstGroup, e.lastGroup, e.tally()), "", ""
		}
		text := e.text + " · passed" + e.tail
		if e.of > 1 || e.runs > 1 {
			text = e.text + " · " + e.tally() + " passed" + e.tail
		}
		if e.peak != nil {
			text += fmt.Sprintf(" · Tctl max %d°C", *e.peak)
		}
		return text, "", ""
	case plainEntry:
		if before, after, ok := strings.Cut(e.text, e.alarm); ok && e.alarm != "" {
			return before, e.alarm, after
		}
	}
	return e.text, "", ""
}

// tally counts merged trials against what their part or requirement needs: "4 of 4", "trials 1-3 of 4". Passes that
// run past the requirement, as when a failure leaves earlier passes standing, are only counted.
func (e entry) tally() string {
	switch {
	case e.of == 0 || e.of == 1 && e.runs == 1 || e.first+e.runs-1 > e.of:
		return fmt.Sprint(e.runs)
	case e.first == 1 && e.runs == e.of:
		return fmt.Sprintf("%d of %d", e.runs, e.of)
	case e.runs == 1:
		return fmt.Sprintf("trial %d of %d", e.first, e.of)
	}
	return fmt.Sprintf("trials %d-%d of %d", e.first, e.first+e.runs-1, e.of)
}

func commaIDs(ids []int) string {
	parts := make([]string, len(ids))
	for i, id := range ids {
		parts[i] = fmt.Sprintf("%02d", id)
	}
	return strings.Join(parts, ", ")
}
