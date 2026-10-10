package main

import (
	"cmp"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"text/tabwriter"
	"time"

	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/render"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/tuner"
)

var statusHelp = `Usage: togi status

Show the session at a glance: search, hunt, phase 2 round or checking activity,
the phase and, once phase 2 concluded, the clean cycles; the BIOS profile to
enter, marked * while unconfirmed; then each core's offset, BIOS offset,
failure point and combinations, phase, queued work and last decision. In phase 2
it lists the candidates with their remaining gaps and the most rounds left if
none fails, then one full cycle. An open hunt shows groups and trials; an open
phase 2 round shows checks and passes. Evidence
includes workloads, valid trials and the Tctl peak since the last profile
change, and lists between-trial MCEs without treating them as failures.
Shows each core's observed self-sufficiency for each R7 workload and its CCD's
current top requesters; offsets stand in only when request telemetry is absent.
Lists reset commands for unanswered too-cautious defects. Read-only; rendered
from the journal. A different ruleset warns before rendering; a different schema
is refused.

` + examples(
	example{"togi status", "The session in the default state directory"},
	example{"togi --state-dir <dir> status", "The session in <dir>, such as a copied state directory"},
)

func runStatus(g *globals, args []string, stdout, stderr io.Writer) int {
	flags := newFlagSet("status", g)
	if code, ok := parseFlags(flags, args, statusHelp, stdout, stderr); !ok {
		return code
	}
	events, st, code, ok := loadSession("status", g.stateDir, stderr)
	if !ok {
		return code
	}
	writeStatus(stdout, st, events)
	return exitOK
}

func replayDir(dir string) ([]journal.Event, journal.State, []byte, error) {
	events, torn, err := journal.Read(dir)
	if err != nil {
		return nil, journal.State{}, nil, err
	}
	var st journal.State
	t := tuner.New()
	journal.Replay(events, &st, t)
	t.Project(&st)
	return events, st, torn, nil
}

func loadSession(name, dir string, stderr io.Writer) ([]journal.Event, journal.State, int, bool) {
	events, st, torn, err := replayDir(dir)
	var incompatible *journal.IncompatibleError
	switch {
	case errors.Is(err, fs.ErrNotExist):
		fmt.Fprintf(stderr, "togi %s: no journal at %s\n", name, filepath.Join(dir, "events.jsonl"))
		return nil, st, exitError, false
	case errors.As(err, &incompatible):
		fmt.Fprintln(stderr, render.NewRenderer(stderr, os.Getenv).Styled(render.RedBold, "togi "+name+": "+incompatible.Error()))
		return nil, st, exitError, false
	case err != nil:
		fmt.Fprintf(stderr, "togi %s: %s\n", name, render.EscapeText(err.Error()))
		return nil, st, exitError, false
	case st.Session == nil:
		fmt.Fprintf(stderr, "togi %s: no session in %s\n", name, dir)
		return nil, st, exitError, false
	}
	if len(events) > 0 {
		warnRuleset(events, stderr)
	}
	if len(torn) > 0 {
		fmt.Fprintf(stderr, "togi %s: journal ends with %d torn bytes; the next run records journal.torn\n", name, len(torn))
	}
	return events, st, exitOK, true
}

func warnRuleset(events []journal.Event, stderr io.Writer) {
	recorded := journal.BuildOf(events)
	binary := session.Build()
	if journal.Classify(recorded, binary).Ruleset != journal.DirSame {
		fmt.Fprintln(stderr, render.NewRenderer(stderr, os.Getenv).Styled(render.Yellow, journal.RulesetWarning(recorded, binary)))
	}
}

func writeStatus(w io.Writer, st journal.State, events []journal.Event) {
	activity := "checking not started"
	switch st.Phase {
	case string(journal.PhaseSearch):
		left := 0
		for _, c := range st.Cores {
			if c.Phase == journal.PhaseSearch {
				left++
			}
		}
		activity = fmt.Sprintf("search: %d cores left", left)
	case string(journal.PhaseHunt):
		if h := st.Hunt; h != nil {
			group := 0
			if len(h.Groups) != 0 {
				group = h.Groups[len(h.Groups)-1].Group
			}
			activity = fmt.Sprintf("hunt %d, group %d", h.Hunt, group)
		}
	case string(journal.PhaseDeepening):
		if r := st.Deepening; r != nil {
			done := 0
			for _, check := range r.Checks {
				if check.Passes >= check.Needed {
					done++
				}
			}
			activity = fmt.Sprintf("phase 2 round %d, checks %d/%d", r.Round, done, len(r.Checks))
		}
	case string(journal.PhaseChecking):
		if gs := st.Checking; gs != nil {
			activity = fmt.Sprintf("checking cycle %d, steps %d/%d", gs.Cycle, gs.StepsDone, len(gs.Steps))
			if !gs.Full && len(gs.Missing) > 0 {
				activity += ", not full: " + strings.Join(gs.Missing, "; ")
			}
		}
	}
	header := render.EscapeText(activity)
	if clause := phaseClause(st); clause != "" {
		header += " | " + clause
	}
	wrapLines(w, "", "  ", header)
	fmt.Fprintf(w, "session %s started %s\n", render.EscapeText(st.Session.ID), st.Session.Start.UTC().Format(time.RFC3339))
	writeBIOSLine(w, st.Session)
	if f := st.InFlight; f != nil {
		wrapLines(w, "", "  ", fmt.Sprintf("in flight: [#%d] %s", f.Seq, render.EscapeText(f.Msg)))
	} else {
		fmt.Fprintln(w, "in flight: none")
	}
	if d := st.DeadEnd; d != nil {
		wrapLines(w, "", "  ", fmt.Sprintf("dead end: %s [#%d]", render.EscapeText(string(d.Condition)), d.Seq))
	}
	for _, e := range events {
		if e.Kind == journal.KindSessionCarried {
			wrapLines(w, "", "  ", fmt.Sprintf("carried: [#%d] %s", e.Seq, render.EscapeText(e.Msg)))
		}
	}

	fmt.Fprintln(w)
	rows := []notedRow{{cells: "CORE\tCCD\tSLOT\tOFFSET\tBIOS\tPHASE\tFAILURE\tQUEUED"}}
	for i, c := range st.Cores {
		var notes []rowNote
		if len(c.Combinations) != 0 {
			notes = append(notes, rowNote{"combinations", strings.ReplaceAll(combinationIDs(c.Combinations), ",", ", ")})
		}
		if d := c.LastDecision; d != nil {
			notes = append(notes, rowNote{"last decision", fmt.Sprintf("[#%d] %s", d.Seq, render.EscapeText(d.Msg))})
		}
		queued := cmp.Or(c.Queued, "-")
		rows = append(rows, notedRow{cells: fmt.Sprintf("%02d\t%d\t%d\t%d\t%s\t%s\t%s\t%s", c.Core, c.CCD, c.Core%8, c.Offset, biosCell(st.BIOS, i, c.Core), render.EscapeText(render.PhaseWord(c.Phase)), failurePoint(c.FailurePoint), render.EscapeText(queued)), notes: notes})
	}
	writeNotedTable(w, rows)
	writeBIOSNote(w, st)
	writeCombinations(w, st.Combinations)
	writeR7Status(w, events)
	for _, e := range events {
		if p, ok := e.Data.(*journal.MCE); ok && p.BetweenTrials {
			wrapLines(w, "\n", "  ", fmt.Sprintf("between-trial evidence [#%d]: %s", e.Seq, render.EscapeText(e.Msg)))
		}
	}
	if h := st.Hunt; h != nil {
		parked := "all-zero"
		if h.ParkedSeq != 0 {
			parked = fmt.Sprintf("cycle end #%d", h.ParkedSeq)
		}
		signal := "failure"
		for _, e := range events {
			if e.Seq == h.Failure {
				switch p := e.Data.(type) {
				case *journal.Failure:
					signal = string(p.Signal)
				case *journal.TrialEnd:
					if p.Outcome == journal.OutcomeFailure {
						signal = string(p.Signal)
					}
				case *journal.TrialCarried:
					if p.Outcome == journal.OutcomeFailure {
						signal = string(p.Signal)
					}
				case *journal.FailureCarried:
					signal = string(p.Signal)
				}
				break
			}
		}
		wrapLines(w, "\n", "  ", fmt.Sprintf("hunt %d [#%d]: unattributed %s in %s trial %s; parked offsets %s", h.Hunt, h.Seq, render.EscapeText(signal), render.EscapeText(string(h.Regime)), render.EscapeText(cmp.Or(h.Trial, "-")), parked))
		wrapLines(w, "  ", "    ", "candidates "+coreIDs(h.Candidates))
		groups := []notedRow{{cells: "GROUP\tCORES\tOUTCOME\tPASS EVIDENCE\tNEED"}}
		for _, m := range h.Groups {
			cores, notes := coreIDs(m.Cores), []rowNote(nil)
			if m.Probe != nil {
				held := make([]string, len(m.Held))
				for i, h := range m.Held {
					held[i] = fmt.Sprintf("%02d at %d", h.Core, h.Offset)
				}
				cores = "probe"
				notes = []rowNote{{"probe", fmt.Sprintf("%02d at %d with %s", m.Probe.Core, m.Probe.Offset, strings.Join(held, ", "))}}
			}
			groups = append(groups, notedRow{cells: fmt.Sprintf("G%d\t%s\t%s\t%d\t%d", m.Group, cores, render.EscapeText(m.Outcome), m.Passes, m.Needed), notes: notes})
		}
		writeNotedTable(w, groups)
	}
	writePhase2(w, st)
	var findings []journal.DefectFound
	for _, event := range events {
		switch p := event.Data.(type) {
		case *journal.DefectFound:
			if defect.Direction(p.Direction) == defect.TooCautious {
				finding := *p
				finding.Cores = slices.Clone(p.Cores)
				findings = append(findings, finding)
			}
		case *journal.CommandReset:
			if p.All {
				findings = nil
			} else if p.Core != nil {
				for i := range findings {
					findings[i].Cores = slices.DeleteFunc(findings[i].Cores, func(core int) bool { return core == *p.Core })
				}
			}
		}
	}
	for _, finding := range findings {
		if len(finding.Cores) == 0 {
			continue
		}
		wrapLines(w, "\n", "  ", fmt.Sprintf("defect %d: %s (fixed by pull request #%d); decisions %v affected cores %v", finding.ID, render.EscapeText(finding.Title), finding.PR, finding.Decisions, finding.Cores))
		for _, core := range finding.Cores {
			fmt.Fprintf(w, "  togi reset --core %d\n", core)
		}
	}

	gs := st.Checking
	if gs == nil {
		return
	}
	fmt.Fprintln(w)
	tw := newTable(w)
	fmt.Fprintln(tw, "REGIME\tWORKLOAD\tTRIALS")
	for _, r := range gs.Exposure {
		fmt.Fprintf(tw, "%s\t%s\t%d\n", render.EscapeText(string(r.Regime)), render.EscapeText(r.Workload), r.Trials)
	}
	_ = tw.Flush()
	if gs.TctlMaxC != nil {
		fmt.Fprintf(w, "Tctl max since last profile change %d°C [#%d]\n", *gs.TctlMaxC, gs.TctlMaxSeq)
	}
}

func writeR7Status(w io.Writer, events []journal.Event) {
	t := tuner.New()
	for _, e := range events {
		t.Fold(e)
	}
	status := t.R7Status()
	if len(status) == 0 {
		return
	}
	wrapLines(w, "\n", "  ", "R7 self-sufficiency (passed as top requester at equal or deeper offsets; not a guarantee)")
	rows := []notedRow{{cells: "CORE\tCCD\tSELF-SUFFICIENT\tTOP REQUESTER"}}
	for _, c := range status {
		self := "not yet demonstrated"
		if c.SelfSufficient {
			self = fmt.Sprintf("observed (%d passing trials)", c.Passes)
		}
		top := "no"
		if c.TopRequester {
			top = "yes"
		}
		if c.OffsetFallback {
			top += " (offset fallback)"
		}
		rows = append(rows, notedRow{cells: fmt.Sprintf("%02d\t%d\t%s\t%s", c.Core, c.CCD, self, top), notes: []rowNote{{"workload", render.EscapeText(c.Workload)}}})
	}
	writeNotedTable(w, rows)
}

func combinationIDs(ids []int) string {
	if len(ids) == 0 {
		return "-"
	}
	parts := make([]string, len(ids))
	for i, id := range ids {
		parts[i] = fmt.Sprintf("C%d", id)
	}
	return strings.Join(parts, ",")
}

func coreIDs(cores []int) string {
	if len(cores) == 0 {
		return "-"
	}
	parts := make([]string, len(cores))
	for i, core := range cores {
		parts[i] = fmt.Sprintf("%02d", core)
	}
	return strings.Join(parts, " ")
}

func writeCombinations(w io.Writer, combinations []journal.CombinationState) {
	if len(combinations) == 0 {
		return
	}
	fmt.Fprintln(w, "\ncombinations:")
	for _, combination := range combinations {
		members := make([]string, len(combination.Members))
		for i, member := range combination.Members {
			members[i] = fmt.Sprintf("core %02d %d", member.Core, member.Offset)
		}
		source := fmt.Sprintf("observed in hunt %d", combination.Hunt)
		if combination.Fallback {
			source = fmt.Sprintf("fallback over every candidate of hunt %d", combination.Hunt)
		}
		wrapLines(w, "  ", "      ", fmt.Sprintf("C%d %s, %s [#%d]", combination.Combination, strings.Join(members, " + "), source, combination.Seq))
	}
}

func writeBIOSLine(w io.Writer, s *journal.SessionInfo) {
	if b := s.BIOSContext; b != nil {
		wrapLines(w, "", "  ", fmt.Sprintf("BIOS %s on %s, %s, microcode %s, boost limit %d MHz", render.EscapeText(b.BIOSVersion), render.EscapeText(b.Board), render.EscapeText(b.CPUModel), render.EscapeText(b.Microcode), b.BoostLimitMHz))
	}
}

func newTable(w io.Writer) *tabwriter.Writer {
	return tabwriter.NewWriter(w, 0, 0, 2, ' ', 0)
}

func failurePoint(p *int) string {
	if p == nil {
		return "-"
	}
	return strconv.Itoa(*p)
}

func phaseClause(st journal.State) string {
	ph := st.Phases
	switch {
	case ph == nil:
		return ""
	case ph.Phase == 1:
		return "phase 1"
	case ph.Phase == 0:
		clean, latest := 0, 0
		if gs := st.Checking; gs != nil {
			clean, latest = gs.CleanCycles, gs.LastCleanCycle
		}
		return fmt.Sprintf("phase 2 concluded: clean cycles %d, latest cycle %d", clean, latest)
	case ph.Confirming:
		return "phase 2: confirmation cycle"
	}
	return fmt.Sprintf("phase 2: at most %d %s left, then one full cycle", ph.RoundsLeft, plural(ph.RoundsLeft, "round"))
}

func plural(n int, word string) string {
	if n == 1 {
		return word
	}
	return word + "s"
}

func biosCell(b *journal.BIOSState, i, core int) string {
	if b == nil || i >= len(b.Offsets) {
		return "-"
	}
	if slices.Contains(b.Unconfirmed, core) {
		return fmt.Sprintf("%d*", b.Offsets[i])
	}
	return fmt.Sprint(b.Offsets[i])
}

func writeBIOSNote(w io.Writer, st journal.State) {
	switch b := st.BIOS; {
	case b == nil && st.Phases != nil && st.Phases.Phase == 1:
		wrapLines(w, "", "  ", "BIOS profile: none confirmed yet; phase 1 has not passed a full cycle")
	case b == nil:
	case len(b.Unconfirmed) == 0:
		wrapLines(w, "", "  ", fmt.Sprintf("BIOS profile confirmed by the passed full cycle [#%d]", b.Confirmed))
	default:
		wrapLines(w, "", "  ", fmt.Sprintf("* unconfirmed since [#%d]: cores %s show a stepped-back offset, the profile confirmed by the passed full cycle [#%d] otherwise; a passed full cycle confirms it, except a held failure's stepped-back offset, which stays marked until the hold ends", b.Since, coreIDs(b.Unconfirmed), b.Confirmed))
	}
}

func writePhase2(w io.Writer, st journal.State) {
	ph := st.Phases
	if r := st.Deepening; r != nil {
		wrapLines(w, "\n", "  ", fmt.Sprintf("phase 2 round %d [#%d]", r.Round, r.Seq))
	}
	if ph != nil && ph.Phase == 2 {
		writeCandidates(w, ph, st.Deepening != nil)
	}
	if r := st.Deepening; r != nil {
		checks := []notedRow{{cells: "CHECK\tCORES\tPASSES"}}
		for _, check := range r.Checks {
			checks = append(checks, notedRow{cells: fmt.Sprintf("%s %s\t%s\t%d/%d", render.EscapeText(string(check.Regime)), render.EscapeText(check.Workload), coreIDs(check.Cores), check.Passes, check.Needed)})
		}
		writeNotedTable(w, checks)
	}
}

func writeCandidates(w io.Writer, ph *journal.PhasesState, open bool) {
	if len(ph.Candidates) > 0 {
		rows := []notedRow{{cells: "CANDIDATE\tOFFSET\tSOLO\tGAP\tSTATE"}}
		for _, c := range ph.Candidates {
			state := "waits"
			switch {
			case c.Moving:
				state = "moving"
			case c.Carried:
				state = "checked again in place"
			}
			rows = append(rows, notedRow{cells: fmt.Sprintf("%02d\t%d\t%d\t%d\t%s", c.Core, c.Offset, c.SoloLimit, c.Gap, state)})
		}
		fmt.Fprintln(w)
		writeNotedTable(w, rows)
	}
	switch {
	case ph.Confirming && !open:
		wrapLines(w, "\n", "  ", "no core can move: the confirmation cycle is all that is left")
	default:
		wrapLines(w, "\n", "  ", fmt.Sprintf("worst case if no round fails: %d %s left, then one full cycle", ph.RoundsLeft, plural(ph.RoundsLeft, "round")))
	}
}
