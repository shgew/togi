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
	"text/tabwriter"
	"time"

	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/tuner"
)

const statusHelp = `Usage: togi status

Show the session at a glance: phase, tier and guard progress, then one row per
core with its offset, regainable and settled depth and last decision. Lists
reset commands for unanswered too-cautious defects. Read-only; rendered from
the journal. A different ruleset warns before rendering; a different schema is
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
		fmt.Fprintf(stderr, "togi %s: %v\n", name, err)
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
	fmt.Fprintf(w, "session %s started %s\n", st.Session.ID, st.Session.Start.UTC().Format(time.RFC3339))
	writeBIOSLine(w, st.Session)
	guardPart := "guard not started"
	if gs := st.Guard; gs != nil {
		guardPart = fmt.Sprintf("rotation %d, %d of %d steps", gs.Rotation, gs.StepsDone, len(gs.Steps))
	}
	fmt.Fprintf(w, "phase %s | tier %s | %s\n", st.Phase, tierRef(st), guardPart)
	if f := st.InFlight; f != nil {
		fmt.Fprintf(w, "in flight: [#%d] %s\n", f.Seq, f.Msg)
	} else {
		fmt.Fprintln(w, "in flight: none")
	}
	if d := st.DeadEnd; d != nil {
		fmt.Fprintf(w, "dead end: %s [#%d]\n", d.Condition, d.Seq)
	}
	for _, e := range events {
		if e.Kind == journal.KindSessionCarried {
			fmt.Fprintf(w, "carried: [#%d] %s\n", e.Seq, e.Msg)
		}
	}

	fmt.Fprintln(w)
	tw := newTable(w)
	fmt.Fprintln(tw, "CORE\tCCD\tOFFSET\tPHASE\tFAILED\tREGAINABLE\tSETTLED\tQUEUED\tLAST DECISION")
	for _, c := range st.Cores {
		last := "-"
		if d := c.LastDecision; d != nil {
			last = fmt.Sprintf("[#%d] %s", d.Seq, d.Msg)
		}
		queued := cmp.Or(c.Queued, "-")
		fmt.Fprintf(tw, "%02d\t%d\t%d\t%s\t%s\t%d\t%d\t%s\t%s\n", c.Core, c.CCD, c.Offset, c.Phase, mark(c.FailedMark), c.UnprovenDepth-c.SettledDepth, c.SettledDepth, queued, last)
	}
	_ = tw.Flush()
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
		fmt.Fprintf(w, "\ndefect %d: %s (fixed by pull request #%d); decisions %v affected cores %v\n", finding.ID, finding.Title, finding.PR, finding.Decisions, finding.Cores)
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
	fmt.Fprintf(w, "clean hours since profile.change [#%d]: %s h, %s\n", gs.ProfileSeq, hours(gs.CleanS), overall)
	tw = newTable(w)
	fmt.Fprintln(tw, "REGIME\tCLEAN H\tRATE BOUND")
	for _, r := range gs.Regimes {
		fmt.Fprintf(tw, "%s\t%s\t%s\n", r.Regime, hours(r.CleanS), rate(r.RateBoundPerH))
	}
	_ = tw.Flush()
	if gs.TctlMaxC != nil {
		fmt.Fprintf(w, "Tctl max on this profile %d°C [#%d]\n", *gs.TctlMaxC, gs.TctlMaxSeq)
	}
}

func writeBIOSLine(w io.Writer, s *journal.SessionInfo) {
	if b := s.BIOSContext; b != nil {
		fmt.Fprintf(w, "BIOS %s on %s, %s, microcode %s, boost limit %d MHz\n", b.BIOSVersion, b.Board, b.CPUModel, b.Microcode, b.BoostLimitMHz)
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
