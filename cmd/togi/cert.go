package main

import (
	"crypto/sha256"
	"fmt"
	"io"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/shgew/togi/internal/journal"
)

const (
	certHelp = `Usage: togi cert

Render the certificate: the tier the current profile earned, each core's offset,
CCD and slot, failed and joint marks and done state, qualifying rotation evidence
since the tier clock with workload starts, clean hours and failure-rate bounds,
and the SHA-256 of the journal lines it was rendered from. Match the offsets to
the BIOS per-core Curve Optimizer controls by CCD and slot. Board labels vary;
stop if they cannot be reconciled. A different ruleset warns before rendering;
a different schema is refused.

Examples:
  togi cert   The certificate of the session in the default state directory`
	certWidth = 62
	certText  = 17
)

var medal = []string{
	"      .---.",
	"     / * * \\",
	"    |   *   |",
	"     \\ * * /",
	"      '---'",
	"       | |",
}

var ladder = []struct {
	tier   journal.Tier
	name   string
	cleanH int
}{
	{journal.TierBronze, "Bronze", 0},
	{journal.TierSilver, "Silver", 24},
	{journal.TierGold, "Gold", 100},
	{journal.TierPlatinum, "Platinum", 0},
}

func runCert(g *globals, args []string, stdout, stderr io.Writer) int {
	flags := newFlagSet("cert", g)
	if code, ok := parseFlags(flags, args, certHelp, stdout, stderr); !ok {
		return code
	}
	events, st, code, ok := loadSession("cert", g.stateDir, stderr)
	if !ok {
		return code
	}
	writeCert(stdout, events, st)
	return exitOK
}

func writeCert(w io.Writer, events []journal.Event, st journal.State) {
	rule := "+" + strings.Repeat("-", certWidth-2) + "+"
	title := "T O G I   C E R T I F I C A T E"
	left := (certWidth - 2 - len(title)) / 2
	fmt.Fprintf(w, "%s\n|%s%s%s|\n%s\n\n", rule, strings.Repeat(" ", left), title, strings.Repeat(" ", certWidth-2-left-len(title)), rule)

	tiers := tierLines(st)
	for i, line := range medal {
		text := ""
		if i > 0 && i-1 < len(tiers) {
			text = tiers[i-1]
		}
		if text == "" {
			fmt.Fprintln(w, line)
			continue
		}
		fmt.Fprintln(w, line+strings.Repeat(" ", certText-len(line))+text)
	}
	fmt.Fprintln(w)

	gs := st.Guard
	if gs == nil {
		fmt.Fprintln(w, "Offsets (no profile under guard yet)")
	} else {
		fmt.Fprintln(w, withRef("Profile", certWidth, journal.KindProfileChange, gs.ProfileSeq))
	}
	tw := newTable(w)
	fmt.Fprintln(tw, "  CORE\tCCD\tSLOT\tOFFSET\tFAILED\tJOINT\tDONE\tDECIDED")
	var moved []string
	for i, c := range st.Cores {
		decided := "-"
		if d := c.LastDecision; d != nil {
			decided = fmt.Sprintf("[#%d]", d.Seq)
		}
		offset := c.Offset
		if gs != nil && i < len(gs.Profile) {
			offset = gs.Profile[i]
		}
		if offset != c.Offset {
			moved = append(moved, fmt.Sprintf("  core %02d is at %d since %s; the next profile.change restarts guard", c.Core, c.Offset, decided))
		}
		fmt.Fprintf(tw, "  %02d\t%d\t%d\t%d\t%s\t%s\t%t\t%s\n", c.Core, c.CCD, c.Core%8, offset, mark(c.FailedMark), jointIDs(c.JointMarks), c.Phase == journal.PhaseDone, decided)
	}
	_ = tw.Flush()
	writeJointMarks(w, st.JointMarks)
	for _, m := range moved {
		fmt.Fprintln(w, m)
	}
	fmt.Fprintln(w)

	if gs == nil {
		fmt.Fprintln(w, "Evidence")
		fmt.Fprintln(w, "  no guard evidence yet")
	} else {
		if clockIsFailure(events, gs.TierClockSeq) {
			fmt.Fprintf(w, "Evidence since [#%d]: the last failure on a profile at least as deep\n", gs.TierClockSeq)
		} else {
			fmt.Fprintf(w, "Evidence since the profile change [#%d]\n", gs.TierClockSeq)
		}
		tw = newTable(w)
		fmt.Fprintln(tw, "  REGIME\tWORKLOAD\tSTARTS\tCLEAN H\tRATE BOUND")
		for _, r := range gs.Exposure {
			fmt.Fprintf(tw, "  %s\t%s\t%d\t%s\t%s\n", journal.EscapeText(string(r.Regime)), journal.EscapeText(r.Workload), r.Starts, hours(r.CleanS), rate(r.RateBoundPerH))
		}
		fmt.Fprintf(tw, "  all\t-\t-\t%s\t%s\n", hours(gs.CleanS), rate(gs.RateBoundPerH))
		_ = tw.Flush()
		if gs.TctlMaxC != nil {
			fmt.Fprintln(w, withRef(fmt.Sprintf("  Tctl max %d°C", *gs.TctlMaxC), certWidth, journal.KindTrialEnd, gs.TctlMaxSeq))
		}
		fmt.Fprintln(w, "  Rate bound = 3 / clean hours, 95% confidence (rule of three).")
		fmt.Fprintln(w, "  Starts = valid passes since each class last failed.")
	}
	fmt.Fprintln(w)

	s := st.Session
	fmt.Fprintf(w, "Session %s, started %s\n", journal.EscapeText(s.ID), s.Start.UTC().Format(time.RFC3339))
	writeBIOSLine(w, s)
	fmt.Fprintln(w)

	sum := sha256.New()
	for _, e := range events {
		sum.Write(e.Raw)
		sum.Write([]byte{'\n'})
	}
	fmt.Fprintf(w, "Journal SHA-256 %x through seq %d\n", sum.Sum(nil), events[len(events)-1].Seq)
	fmt.Fprintln(w, "Durability is a bound, not proof.")
}

func clockIsFailure(events []journal.Event, seq int) bool {
	for _, e := range events {
		if e.Seq == seq {
			return e.Kind == journal.KindFailure
		}
	}
	return false
}

func tierLines(st journal.State) []string {
	clean := 0
	if st.Guard != nil {
		clean = st.Guard.CleanS
	}
	reached := -1
	for i, l := range ladder {
		if l.tier == st.Tier {
			reached = i
		}
	}
	title := "NO TIER YET"
	if reached >= 0 {
		title = withRef(strings.ToUpper(ladder[reached].name), certWidth-certText, journal.KindTierChange, st.TierSeq)
	}
	lines := []string{title}
	for i, l := range ladder {
		var what string
		switch {
		case i == reached:
			continue
		case l.tier == journal.TierPlatinum:
			what = "locked until togi observe exists"
		case i < reached:
			what = "earned"
		case l.tier == journal.TierBronze:
			what = "every core done and one clean qualifying rotation since the last core went deeper"
		default:
			what = fmt.Sprintf("%.1f of %d clean hours", float64(clean/360)/10, l.cleanH)
		}
		lines = append(lines, fmt.Sprintf("%-10s%s", l.name, what))
	}
	return lines
}

func withRef(left string, width int, kind journal.Kind, seq int) string {
	ref := fmt.Sprintf("[%s #%d]", kind, seq)
	pad := max(width-utf8.RuneCountInString(left)-len(ref), 1)
	return left + strings.Repeat(" ", pad) + ref
}
