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

const statusHelp = `Usage: togi status

Show the session at a glance: search, hunt, deepening or checking activity and
clean cycles since the last deepening, then each core's offset, failure point
and combinations, phase, queued work and last decision. An open hunt shows
groups and trials; an open deepening round shows checks and passes. Evidence
includes workloads, valid trials and the Tctl peak since the last profile
change, and lists between-trial MCEs without treating them as failures.
Shows each core's observed self-sufficiency for each R7 workload and its CCD's
current top requesters; offsets stand in only when request telemetry is absent.
Lists reset commands for unanswered too-cautious defects. Read-only; rendered
from the journal. A different ruleset warns before rendering; a different schema
is refused.

Examples:
  togi status                     The session in the default state directory
  togi --state-dir <dir> status   The session in <dir>, such as a copied state directory`

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
	if recorded.Ruleset != binary.Ruleset {
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
			activity = fmt.Sprintf("deepening round %d, checks %d/%d", r.Round, done, len(r.Checks))
		}
	case string(journal.PhaseChecking):
		if gs := st.Checking; gs != nil {
			activity = fmt.Sprintf("checking cycle %d, steps %d/%d", gs.Cycle, gs.StepsDone, len(gs.Steps))
			if !gs.Full && len(gs.Missing) > 0 {
				activity += ", not full: " + strings.Join(gs.Missing, "; ")
			}
		}
	}
	clean, latest := 0, 0
	if gs := st.Checking; gs != nil {
		clean, latest = gs.CleanCycles, gs.LastCleanCycle
	}
	fmt.Fprintf(w, "%s | clean cycles since last deepening: %d, latest cycle %d\n", render.EscapeText(activity), clean, latest)
	fmt.Fprintf(w, "session %s started %s\n", render.EscapeText(st.Session.ID), st.Session.Start.UTC().Format(time.RFC3339))
	writeBIOSLine(w, st.Session)
	if f := st.InFlight; f != nil {
		fmt.Fprintf(w, "in flight: [#%d] %s\n", f.Seq, render.EscapeText(f.Msg))
	} else {
		fmt.Fprintln(w, "in flight: none")
	}
	if d := st.DeadEnd; d != nil {
		fmt.Fprintf(w, "dead end: %s [#%d]\n", render.EscapeText(string(d.Condition)), d.Seq)
	}
	for _, e := range events {
		if e.Kind == journal.KindSessionCarried {
			fmt.Fprintf(w, "carried: [#%d] %s\n", e.Seq, render.EscapeText(e.Msg))
		}
	}

	fmt.Fprintln(w)
	tw := newTable(w)
	fmt.Fprintln(tw, "CORE\tCCD\tSLOT\tOFFSET\tPHASE\tFAILURE\tCOMBINATIONS\tQUEUED\tLAST DECISION")
	for _, c := range st.Cores {
		last := "-"
		if d := c.LastDecision; d != nil {
			last = fmt.Sprintf("[#%d] %s", d.Seq, render.EscapeText(d.Msg))
		}
		queued := cmp.Or(c.Queued, "-")
		fmt.Fprintf(tw, "%02d\t%d\t%d\t%d\t%s\t%s\t%s\t%s\t%s\n", c.Core, c.CCD, c.Core%8, c.Offset, render.EscapeText(render.PhaseWord(c.Phase)), failurePoint(c.FailurePoint), combinationIDs(c.Combinations), render.EscapeText(queued), last)
	}
	_ = tw.Flush()
	writeCombinations(w, st.Combinations)
	writeR7Status(w, events)
	for _, e := range events {
		if p, ok := e.Data.(*journal.MCE); ok && p.BetweenTrials {
			fmt.Fprintf(w, "\nbetween-trial evidence [#%d]: %s\n", e.Seq, render.EscapeText(e.Msg))
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
		fmt.Fprintf(w, "\nhunt %d [#%d]: unattributed %s in %s trial %s; parked offsets %s\n", h.Hunt, h.Seq, render.EscapeText(signal), render.EscapeText(string(h.Regime)), render.EscapeText(cmp.Or(h.Trial, "-")), parked)
		fmt.Fprintf(w, "  candidates %s\n", coreIDs(h.Candidates))
		tw := newTable(w)
		fmt.Fprintln(tw, "GROUP\tCORES\tOUTCOME\tTRIALS")
		for _, m := range h.Groups {
			cores := coreIDs(m.Cores)
			if m.Probe != nil {
				held := make([]string, len(m.Held))
				for i, h := range m.Held {
					held[i] = fmt.Sprintf("%02d at %d", h.Core, h.Offset)
				}
				cores = fmt.Sprintf("%02d at %d with %s", m.Probe.Core, m.Probe.Offset, strings.Join(held, ", "))
			}
			fmt.Fprintf(tw, "G%d\t%s\t%s\t%d/%d\n", m.Group, cores, render.EscapeText(m.Outcome), m.Passes, m.Needed)
		}
		_ = tw.Flush()
	}
	if r := st.Deepening; r != nil {
		fmt.Fprintf(w, "\ndeepening round %d [#%d]: target %v; proposed %v\n", r.Round, r.Seq, r.Target, r.Profile)
		tw := newTable(w)
		fmt.Fprintln(tw, "CHECK\tCORES\tPASSES")
		for _, check := range r.Checks {
			fmt.Fprintf(tw, "%s %s\t%s\t%d/%d\n", render.EscapeText(string(check.Regime)), render.EscapeText(check.Workload), coreIDs(check.Cores), check.Passes, check.Needed)
		}
		_ = tw.Flush()
	}
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
		fmt.Fprintf(w, "\ndefect %d: %s (fixed by pull request #%d); decisions %v affected cores %v\n", finding.ID, render.EscapeText(finding.Title), finding.PR, finding.Decisions, finding.Cores)
		for _, core := range finding.Cores {
			fmt.Fprintf(w, "  togi reset --core %d\n", core)
		}
	}

	gs := st.Checking
	if gs == nil {
		return
	}
	fmt.Fprintln(w)
	tw = newTable(w)
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
	fmt.Fprintln(w, "\nR7 self-sufficiency (passed as top requester at equal or deeper offsets; not a guarantee)")
	tw := newTable(w)
	fmt.Fprintln(tw, "CORE\tCCD\tWORKLOAD\tSELF-SUFFICIENT\tTOP REQUESTER")
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
		fmt.Fprintf(tw, "%02d\t%d\t%s\t%s\t%s\n", c.Core, c.CCD, render.EscapeText(c.Workload), self, top)
	}
	_ = tw.Flush()
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
		fmt.Fprintf(w, "  C%d %s, %s [#%d]\n", combination.Combination, strings.Join(members, " + "), source, combination.Seq)
	}
}

func writeBIOSLine(w io.Writer, s *journal.SessionInfo) {
	if b := s.BIOSContext; b != nil {
		fmt.Fprintf(w, "BIOS %s on %s, %s, microcode %s, boost limit %d MHz\n", render.EscapeText(b.BIOSVersion), render.EscapeText(b.Board), render.EscapeText(b.CPUModel), render.EscapeText(b.Microcode), b.BoostLimitMHz)
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
