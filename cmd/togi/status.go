package main

import (
	"cmp"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"math"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"text/tabwriter"
	"time"

	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/tuner"
)

const statusHelp = `Usage: togi status

Show the session at a glance: search, hunt, refinement or guard activity, tier
and qualifying rotation progress, then each core's offset, failed and joint
marks, done phase, queued work and last decision. An open hunt shows masks and
starts; an open refinement round shows checks and passes. Evidence includes
workloads, starts, clean hours and bounds since the tier clock, and lists
between-trial MCEs without treating them as failures. Lists reset
commands for unanswered too-cautious defects. Read-only; rendered from the
journal. A different ruleset warns before rendering; a different schema is
refused.

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
		fmt.Fprintln(stderr, journal.NewRenderer(stderr, os.Getenv).Styled(journal.RedBold, "togi "+name+": "+incompatible.Error()))
		return nil, st, exitError, false
	case err != nil:
		fmt.Fprintf(stderr, "togi %s: %s\n", name, journal.EscapeText(err.Error()))
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
		fmt.Fprintln(stderr, journal.NewRenderer(stderr, os.Getenv).Styled(journal.Yellow, journal.RulesetWarning(recorded, binary)))
	}
}

func writeStatus(w io.Writer, st journal.State, events []journal.Event) {
	activity := "guard not started"
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
			mask := 0
			if len(h.Masks) != 0 {
				mask = h.Masks[len(h.Masks)-1].Mask
			}
			activity = fmt.Sprintf("hunt %d, mask %d", h.Hunt, mask)
		}
	case string(journal.PhaseRefine):
		if r := st.Refine; r != nil {
			done := 0
			for _, check := range r.Checks {
				if check.Passes >= check.Needed {
					done++
				}
			}
			activity = fmt.Sprintf("refine round %d, checks %d/%d", r.Round, done, len(r.Checks))
		}
	case string(journal.PhaseGuard):
		if gs := st.Guard; gs != nil {
			activity = fmt.Sprintf("guard rotation %d, steps %d/%d", gs.Rotation, gs.StepsDone, len(gs.Steps))
			if !gs.Qualifying && len(gs.Missing) > 0 {
				activity += ", not qualifying: " + strings.Join(gs.Missing, "; ")
			}
		}
	}
	fmt.Fprintf(w, "%s | tier %s\n", journal.EscapeText(activity), journal.EscapeText(tierRef(st)))
	fmt.Fprintf(w, "session %s started %s\n", journal.EscapeText(st.Session.ID), st.Session.Start.UTC().Format(time.RFC3339))
	writeBIOSLine(w, st.Session)
	if f := st.InFlight; f != nil {
		fmt.Fprintf(w, "in flight: [#%d] %s\n", f.Seq, journal.EscapeText(f.Msg))
	} else {
		fmt.Fprintln(w, "in flight: none")
	}
	if d := st.DeadEnd; d != nil {
		fmt.Fprintf(w, "dead end: %s [#%d]\n", journal.EscapeText(string(d.Condition)), d.Seq)
	}
	for _, e := range events {
		if e.Kind == journal.KindSessionCarried {
			fmt.Fprintf(w, "carried: [#%d] %s\n", e.Seq, journal.EscapeText(e.Msg))
		}
	}

	fmt.Fprintln(w)
	tw := newTable(w)
	fmt.Fprintln(tw, "CORE\tCCD\tOFFSET\tPHASE\tFAILED\tJOINT\tQUEUED\tLAST DECISION")
	for _, c := range st.Cores {
		last := "-"
		if d := c.LastDecision; d != nil {
			last = fmt.Sprintf("[#%d] %s", d.Seq, journal.EscapeText(d.Msg))
		}
		queued := cmp.Or(c.Queued, "-")
		fmt.Fprintf(tw, "%02d\t%d\t%d\t%s\t%s\t%s\t%s\t%s\n", c.Core, c.CCD, c.Offset, journal.EscapeText(string(c.Phase)), mark(c.FailedMark), jointIDs(c.JointMarks), journal.EscapeText(queued), last)
	}
	_ = tw.Flush()
	writeJointMarks(w, st.JointMarks)
	for _, e := range events {
		if p, ok := e.Data.(*journal.MCE); ok && p.BetweenTrials {
			fmt.Fprintf(w, "\nbetween-trial evidence [#%d]: %s\n", e.Seq, journal.EscapeText(e.Msg))
		}
	}
	if h := st.Hunt; h != nil {
		anchor := "all-zero"
		if h.AnchorSeq != 0 {
			anchor = fmt.Sprintf("rotation end #%d", h.AnchorSeq)
		}
		signal := "failure"
		for _, e := range events {
			if e.Seq == h.Failure {
				if p, ok := e.Data.(*journal.Failure); ok {
					signal = string(p.Signal)
				}
				break
			}
		}
		fmt.Fprintf(w, "\nhunt %d [#%d]: unattributed %s in %s trial %s; anchor %s\n", h.Hunt, h.Seq, journal.EscapeText(signal), journal.EscapeText(string(h.Regime)), journal.EscapeText(cmp.Or(h.Trial, "-")), anchor)
		fmt.Fprintf(w, "  candidates %s\n", coreIDs(h.Candidates))
		tw := newTable(w)
		fmt.Fprintln(tw, "MASK\tCORES\tOUTCOME\tSTARTS")
		for _, m := range h.Masks {
			cores := coreIDs(m.Cores)
			if m.Edge != nil {
				held := make([]string, len(m.Held))
				for i, h := range m.Held {
					held[i] = fmt.Sprintf("%02d at %d", h.Core, h.Offset)
				}
				cores = fmt.Sprintf("%02d at %d with %s", m.Edge.Core, m.Edge.Offset, strings.Join(held, ", "))
			}
			fmt.Fprintf(tw, "M%d\t%s\t%s\t%d/%d\n", m.Mask, cores, journal.EscapeText(m.Outcome), m.Passes, m.Needed)
		}
		_ = tw.Flush()
	}
	if r := st.Refine; r != nil {
		fmt.Fprintf(w, "\nrefine round %d [#%d]: target %v; proposed %v\n", r.Round, r.Seq, r.Target, r.Profile)
		tw := newTable(w)
		fmt.Fprintln(tw, "CHECK\tCORES\tPASSES")
		for _, check := range r.Checks {
			fmt.Fprintf(tw, "%s %s\t%s\t%d/%d\n", journal.EscapeText(string(check.Regime)), journal.EscapeText(check.Workload), coreIDs(check.Cores), check.Passes, check.Needed)
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
		fmt.Fprintf(w, "\ndefect %d: %s (fixed by pull request #%d); decisions %v affected cores %v\n", finding.ID, journal.EscapeText(finding.Title), finding.PR, finding.Decisions, finding.Cores)
		for _, core := range finding.Cores {
			fmt.Fprintf(w, "  togi reset --core %d\n", core)
		}
	}

	gs := st.Guard
	if gs == nil {
		return
	}
	fmt.Fprintln(w)
	overall := "no failure-rate bound yet"
	if gs.RateBoundPerH != nil {
		overall = fmt.Sprintf("failure rate %s at 95%%", rate(gs.RateBoundPerH))
	}
	fmt.Fprintf(w, "clean hours since the tier clock [#%d]: %s h, %s\n", gs.TierClockSeq, hours(gs.CleanS), overall)
	tw = newTable(w)
	fmt.Fprintln(tw, "REGIME\tWORKLOAD\tSTARTS\tCLEAN H\tRATE BOUND")
	for _, r := range gs.Exposure {
		fmt.Fprintf(tw, "%s\t%s\t%d\t%s\t%s\n", journal.EscapeText(string(r.Regime)), journal.EscapeText(r.Workload), r.Starts, hours(r.CleanS), rate(r.RateBoundPerH))
	}
	_ = tw.Flush()
	if gs.TctlMaxC != nil {
		fmt.Fprintf(w, "Tctl max among counted trials %d°C [#%d]\n", *gs.TctlMaxC, gs.TctlMaxSeq)
	}
}

func jointIDs(ids []int) string {
	if len(ids) == 0 {
		return "-"
	}
	parts := make([]string, len(ids))
	for i, id := range ids {
		parts[i] = fmt.Sprintf("J%d", id)
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

func writeJointMarks(w io.Writer, marks []journal.JointMarkState) {
	if len(marks) == 0 {
		return
	}
	fmt.Fprintln(w, "\njoint marks:")
	for _, mark := range marks {
		members := make([]string, len(mark.Members))
		for i, member := range mark.Members {
			members[i] = fmt.Sprintf("core %02d %d", member.Core, member.Offset)
		}
		source := fmt.Sprintf("observed in hunt %d", mark.Hunt)
		if mark.Fallback {
			source = fmt.Sprintf("fallback over every candidate of hunt %d", mark.Hunt)
		}
		fmt.Fprintf(w, "  J%d %s, %s [#%d]\n", mark.Mark, strings.Join(members, " + "), source, mark.Seq)
	}
}

func writeBIOSLine(w io.Writer, s *journal.SessionInfo) {
	if b := s.BIOSContext; b != nil {
		fmt.Fprintf(w, "BIOS %s on %s, %s, microcode %s, boost limit %d MHz\n", journal.EscapeText(b.BIOSVersion), journal.EscapeText(b.Board), journal.EscapeText(b.CPUModel), journal.EscapeText(b.Microcode), b.BoostLimitMHz)
	}
}

func newTable(w io.Writer) *tabwriter.Writer {
	return tabwriter.NewWriter(w, 0, 0, 2, ' ', 0)
}

func tierRef(st journal.State) string {
	if st.TierSeq == 0 {
		return string(st.Tier)
	}
	return fmt.Sprintf("%s [#%d]", st.Tier, st.TierSeq)
}

func mark(p *int) string {
	if p == nil {
		return "-"
	}
	return strconv.Itoa(*p)
}

func hours(s int) string {
	return strconv.FormatFloat(float64(s)/3600, 'f', 1, 64)
}

func rate(bound *float64) string {
	if bound == nil {
		return "-"
	}
	return fmt.Sprintf("< %.2f/h", math.Ceil(*bound*100-1e-9)/100)
}
