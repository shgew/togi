package main

import (
	"crypto/sha256"
	"fmt"
	"io"
	"strings"
	"time"
	"unicode/utf8"

	"code.marleb.org/shgew/shycler/internal/journal"
)

const (
	certUsage = "Usage: shycler cert"
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
	if code, ok := parseFlags(flags, args, certUsage, stdout, stderr); !ok {
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
	title := "S H Y C L E R   C E R T I F I C A T E"
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
	fmt.Fprintln(tw, "  CORE\tEDGE\tFAILED\tUNPROVEN\tDECIDED")
	for _, c := range st.Cores {
		decided := "-"
		if d := c.LastDecision; d != nil {
			decided = fmt.Sprintf("[#%d]", d.Seq)
		}
		fmt.Fprintf(tw, "  %02d\t%d\t%s\t%d\t%s\n", c.Core, c.Offset, mark(c.FailedMark), c.UnprovenDepth, decided)
	}
	_ = tw.Flush()
	fmt.Fprintln(w)

	if gs == nil {
		fmt.Fprintln(w, "Evidence")
		fmt.Fprintln(w, "  no guard evidence yet")
	} else {
		fmt.Fprintln(w, "Evidence since the profile change")
		tw = newTable(w)
		fmt.Fprintln(tw, "  REGIME\tCLEAN H\tRATE BOUND")
		for _, r := range gs.Regimes {
			fmt.Fprintf(tw, "  %s\t%s\t%s\n", r.Regime, hours(r.CleanS), rate(r.RateBoundPerH))
		}
		fmt.Fprintf(tw, "  all\t%s\t%s\n", hours(gs.CleanS), rate(gs.RateBoundPerH))
		_ = tw.Flush()
		if gs.TctlMaxC != nil {
			fmt.Fprintln(w, withRef(fmt.Sprintf("  Tctl max %d°C", *gs.TctlMaxC), certWidth, journal.KindTrialEnd, gs.TctlMaxSeq))
		}
		fmt.Fprintln(w, "  Rate bound = 3 / clean hours, 95% confidence (rule of three).")
	}
	fmt.Fprintln(w)

	s := st.Session
	fmt.Fprintf(w, "Session %s, started %s\n", s.ID, s.Start.UTC().Format(time.RFC3339))
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
			what = "locked until shycler observe exists"
		case i < reached:
			what = "earned"
		case l.tier == journal.TierBronze:
			what = "every core confirmed and one clean rotation"
		default:
			what = fmt.Sprintf("%s of %d clean hours", hours(clean), l.cleanH)
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
