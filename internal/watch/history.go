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

var regimeExplained = map[machine.Regime]string{
	machine.R1: "one thread of light work, which lets a core boost highest",
	machine.R2: "one thread of heavy vector math, which draws the most current",
	machine.R3: "a load switched on and off in bursts, so the voltage has to follow sudden swings",
	machine.R4: "a load that runs only part of the time",
	machine.R5: "work on both threads of a core at once",
	machine.R6: "the machine mostly idle, with short bursts, the way it sits on a desktop",
	machine.R7: "heavy work on many cores at once, for power and heat",
}

var signalWords = map[machine.Signal]string{
	machine.ComputationError: "wrong result",
	machine.UnexpectedExit:   "the stress program quit",
	machine.Stall:            "the stress program stalled",
	machine.CorrectedMCE:     "corrected hardware error",
	machine.UncorrectedMCE:   "uncorrected hardware error",
	machine.Crash:            "crash",
}

// The tags of what happened: one short word per kind of event, in a column of their own, so the eye finds a crash
// without reading the sentence beside it.
const (
	tagStart   = "start"
	tagPass    = "pass"
	tagFail    = "fail"
	tagCrash   = "crash"
	tagUnclear = "unclear"
	tagCause   = "cause"
	tagSkip    = "skip"
	tagReset   = "reset"
	tagSearch  = "search"
	tagLimit   = "limit"
	tagDeeper  = "deeper"
	tagBackoff = "backoff"
	tagYield   = "yield"
	tagCycle     = "cycle"
	tagHunt    = "hunt"
	tagCombo   = "combo"
	tagRound   = "round"
	tagStop    = "stop"
	tagMCE     = "mce"
	tagNote    = "note"
	tagWidth   = 7
)

func signalText(sig machine.Signal) string {
	if w, ok := signalWords[sig]; ok {
		return w
	}
	return vtText(strings.ReplaceAll(string(sig), "_", " "))
}

// describe turns one journal event into a line of what happened.
func (p *projector) describe(e journal.Event) (entry, bool) {
	n := len(p.st.Cores)
	line := entry{at: e.Time, tone: plainTone}
	switch d := e.Data.(type) {
	case *journal.SessionStart:
		line.tag, line.text = tagStart, fmt.Sprintf("session started on %d cores", len(d.Cores))
	case *journal.TrialEnd:
		return p.trialEnd(line, d, e.Cause)
	case *journal.Failure:
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
	case *journal.TunerDecision:
		line.tag, line.text, line.tone = decisionText(d)
	case *journal.CorePhase:
		line.tag, line.text, line.tone = phaseText(d)
	case *journal.CheckingCycle:
		line.tag, line.text, line.tone = cycleText(d)
	case *journal.HuntStart:
		line.tag, line.text, line.tone = tagHunt, fmt.Sprintf("#%d started: which cores caused it?", d.Hunt), warnTone
	case *journal.HuntEnd:
		line.tag, line.text, line.tone = huntEndText(d)
	case *journal.HuntSkipped:
		line.tag, line.text = tagHunt, "not needed, these offsets already reach a known failure"
	case *journal.Combination:
		text := membersText(d.Members, n)
		if d.Fallback {
			text += " kept as an unresolved, conservative limit"
		} else {
			text += " fail together"
		}
		line.tag, line.text, line.tone = tagCombo, text, warnTone
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
	return line, line.text != ""
}

// trialEnd tells how a trial ended in plain words; the workload's program and settings stay on the running test's line
// and in the event log, so a line of what happened fits the frame.
func (p *projector) trialEnd(line entry, d *journal.TrialEnd, cause []int) (entry, bool) {
	in := p.intents[d.Trial]
	var what string
	if in != nil {
		if in.Condition == machine.Alone && d.Outcome == journal.OutcomePass {
			return entry{}, false
		}
		what = p.trialWhat(in)
	} else {
		what = "trial " + vtText(d.Trial)
	}
	switch d.Outcome {
	case journal.OutcomePass:
		line.tag, line.text, line.tone = tagPass, what, goodTone
		line.runs, line.each, line.peak = 1, time.Duration(d.DurationS)*time.Second, d.TctlMaxC
	case journal.OutcomeFailure:
		recorded := ""
		if in != nil && in.RecordOnly {
			recorded = ", recorded only"
		}
		if d.Signal == machine.Crash {
			line.tag, line.text, line.tone = tagCrash, what+", rebooted"+recorded, badTone
			// Recovery appends the trial outcome after the reboot; only an exact cause links their history.
			for i := len(p.s.history) - 1; i >= 0; i-- {
				previous := &p.s.history[i]
				if previous.reboot != 0 && slices.Contains(cause, previous.reboot) {
					previous.tag, previous.text, previous.tone = line.tag, line.text, line.tone
					return entry{}, false
				}
			}
		} else {
			tone := badTone
			if recorded != "" {
				tone = warnTone
			}
			line.tag, line.text, line.tone = tagFail, what+", "+signalText(d.Signal)+recorded, tone
		}
	case journal.OutcomeInconclusive:
		reason := "it could not run"
		if d.Reason != "" {
			reason = vtText(d.Reason)
		}
		line.tag, line.text, line.tone = tagUnclear, what+", "+reason, warnTone
	}
	return line, true
}

func failureText(d *journal.Failure) (string, string, tone) {
	switch {
	case d.KnownFailure != 0:
		return tagSkip, "this test already failed at these offsets, acting on that", warnTone
	case d.Attribution == journal.Attributed && d.Core != nil && d.Offset != nil:
		return tagCause, fmt.Sprintf("core %02d fails at %d", *d.Core, *d.Offset), warnTone
	case d.Trial == "" && d.Signal != machine.Crash:
		return tagFail, "idle with the offsets applied, " + signalText(d.Signal), badTone
	}
	return "", "", plainTone
}

// trialLabel names a trial's workload; all-core runs use their base workload's name.
func trialLabel(in *journal.TrialIntent) string {
	if in.Regime == machine.R7 {
		return baseLabel(in.Workload)
	}
	return workloadLabel(in.Workload)
}

// baseLabel names a workload by the R1 or R2 workload it derives from, so an all-core run is not labelled twice.
func baseLabel(id string) string {
	if w, ok := machine.WorkloadByID(id); ok && w.Base != "" {
		if b, ok := machine.WorkloadByID(w.Base); ok {
			return b.Label
		}
	}
	return workloadLabel(id)
}

func decisionText(d *journal.TunerDecision) (string, string, tone) {
	switch d.Decision {
	case journal.StepDeeper:
		return tagSearch, fmt.Sprintf("core %02d passed %d, next %d", d.Core, d.FromOffset, d.ToOffset), plainTone
	case journal.CheckSoloLimit:
		return tagSearch, fmt.Sprintf("core %02d confirming %d as its solo limit", d.Core, d.ToOffset), plainTone
	case journal.Deepen:
		return tagDeeper, fmt.Sprintf("core %02d %d → %d", d.Core, d.FromOffset, d.ToOffset), goodTone
	case journal.Yield:
		return tagYield, fmt.Sprintf("core %02d %d → %d so others go deeper", d.Core, d.FromOffset, d.ToOffset), plainTone
	case journal.Backoff:
	}
	return tagBackoff, fmt.Sprintf("core %02d %d → %d", d.Core, d.FromOffset, d.ToOffset), warnTone
}

// phaseText tells when a core finds its solo limit or starts its search over; the other changes show on its row.
func phaseText(d *journal.CorePhase) (string, string, tone) {
	switch {
	case d.To == journal.PhaseSearch && d.From != "":
		return tagSearch, fmt.Sprintf("core %02d starts over from %d", d.Core, d.Offset), warnTone
	case d.From == journal.PhaseSearch:
		return tagLimit, fmt.Sprintf("core %02d solo limit %d", d.Core, d.Offset), goodTone
	}
	return "", "", plainTone
}

func cycleText(d *journal.CheckingCycle) (string, string, tone) {
	switch {
	case d.Event == journal.CycleStart:
		return tagCycle, fmt.Sprintf("#%d started, %d steps", d.Cycle, len(d.Steps)), plainTone
	case d.Passed && d.Full:
		return tagCycle, fmt.Sprintf("#%d passed, a full cycle of every kind of test", d.Cycle), goodTone
	case d.Passed:
		return tagCycle, fmt.Sprintf("#%d passed but missing %s", d.Cycle, vtText(strings.Join(d.Missing, ", "))), warnTone
	}
	return tagCycle, fmt.Sprintf("#%d ended early: %s", d.Cycle, vtText(d.Reason)), warnTone
}

func huntEndText(d *journal.HuntEnd) (string, string, tone) {
	switch d.Result {
	case "culprit", "direct":
		if len(d.Cores) == 1 {
			return tagHunt, fmt.Sprintf("#%d found the culprit: core %02d", d.Hunt, d.Cores[0]), goodTone
		}
	case "combination":
		return tagHunt, fmt.Sprintf("#%d found a combination", d.Hunt), warnTone
	case "fallback":
		return tagHunt, fmt.Sprintf("#%d unresolved: conservative limit over %s", d.Hunt, coreList(d.Cores)), warnTone
	case "cancelled":
		return tagHunt, fmt.Sprintf("#%d cancelled", d.Hunt), plainTone
	}
	return tagHunt, fmt.Sprintf("#%d ended: %s", d.Hunt, vtText(d.Reason)), plainTone
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

// trialWhat says in plain words what a trial loads and where, such as "heavy vector load on core 07 alone at -32". A
// profile lists offsets in the session's core order, which need not match core IDs.
func (p *projector) trialWhat(in *journal.TrialIntent) string {
	what := regimeWords[in.Regime]
	if what == "" {
		what = vtText(string(in.Regime))
	}
	cores := in.Cores
	if in.Core != nil {
		cores = []int{*in.Core}
	}
	where := "on " + coresText(cores, len(p.st.Cores))
	switch {
	case in.Condition == machine.Alone && in.Offset != nil:
		where += fmt.Sprintf(" alone at %d", *in.Offset)
	case len(cores) == 1:
		if i := slices.IndexFunc(p.st.Cores, func(c journal.CoreState) bool { return c.Core == cores[0] }); i >= 0 && i < len(in.Profile) {
			where += fmt.Sprintf(" at %d", in.Profile[i])
		}
	}
	if in.RecordOnly {
		what = "partial " + what
	}
	return what + " " + where
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

func membersText(members []journal.CombinationMember, total int) string {
	if len(members) > 4 {
		cores := make([]int, len(members))
		for i, m := range members {
			cores[i] = m.Core
		}
		return coresText(cores, total) + " at their offsets"
	}
	parts := make([]string, len(members))
	for i, m := range members {
		parts[i] = fmt.Sprintf("core %02d at %d", m.Core, m.Offset)
	}
	return strings.Join(parts, " and ")
}

func duration(d time.Duration) string {
	switch {
	case d < 2*time.Minute:
		return fmt.Sprintf("%d s", int(d.Seconds()))
	case d < time.Hour:
		return fmt.Sprintf("%d min", int(d.Minutes()))
	}
	return hm(d)
}

// sentence is the line as the screen shows it: a pass names how many runs it folds and the hottest Tctl they reached.
func (e entry) sentence() string {
	if e.tag != tagPass {
		return e.text
	}
	text := e.text + ", " + duration(e.each)
	if e.runs > 1 {
		text = fmt.Sprintf("%s, %d runs of %s", e.text, e.runs, duration(e.each))
	}
	if e.peak != nil {
		text += fmt.Sprintf(", peak %d C", *e.peak)
	}
	return text
}
