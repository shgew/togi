package main

import (
	"fmt"
	"io"
	"strconv"
	"strings"
	"time"

	"github.com/shgew/togi/tools/forecast"
)

// scoreForecast scores the run recorded in stateDir after the forecast's anchor.
func scoreForecast(out io.Writer, stateDir, path string) error {
	record, err := forecast.ReadRecord(path)
	if err != nil {
		return err
	}
	sessions, err := forecast.ReadDir(stateDir, record.Anchor.Session)
	if err != nil {
		return err
	}
	score, err := record.Score(sessions)
	if err != nil {
		return fmt.Errorf("stats: score forecast %s: %w", path, err)
	}
	return renderScore(out, score)
}

func renderScore(out io.Writer, s forecast.Score) error {
	tab := &table{out: out}
	r, sum := s.Record, s.Record.Summary
	fmt.Fprintf(tab, "Forecast: commit %s, ruleset %d, %d runs: %d concluded, %d dead ends, %d censored\n", r.Commit, r.Ruleset, sum.Runs, sum.Concluded, sum.DeadEnds, sum.Censored)
	fmt.Fprintf(tab, "Anchor: session %s event %d at %s, unchanged\n", r.Anchor.Session, r.Anchor.Seq, r.Anchor.Time.UTC().Format(time.RFC3339))
	builds := make([]string, len(s.Builds))
	for i, b := range s.Builds {
		builds[i] = fmt.Sprintf("rev %s ruleset %d", b.Rev, b.Ruleset)
	}
	if len(builds) == 0 {
		builds = []string{"none"}
	}
	fmt.Fprintf(tab, "Run: %s %.2f h after the anchor; builds after the anchor: %s\n", s.Outcome.Status, s.Outcome.Hours, strings.Join(builds, ", "))
	for _, flag := range s.Flags {
		fmt.Fprintf(tab, "Flag: %s\n", flag)
	}
	if s.Outcome.Status == forecast.Censored {
		fmt.Fprintln(tab, "Censored: the run has not concluded, so its hours, crashes and hunts are lower bounds, and offsets and depth are those it stopped at")
	}
	fmt.Fprintf(tab, "Note: forecast hours run to conclusion over concluded runs only; %s\n", forecast.RecoveryBias())
	tab.section("\nForecast score", "metric\tactual\tinside\terror\tpercentile\tmedian\tp10..p90\tmin..max")
	for _, row := range s.Rows {
		m := row.Metric
		bound := ""
		if row.Bound {
			bound = ">="
		}
		if m.Range == nil {
			tab.row("%s\t%s%s\t%s\t-\t-\t-\t-\t-", m.Name, bound, m.Format(row.Actual), row.Placement)
			continue
		}
		errorText := m.Format(row.Error)
		if row.Error > 0 {
			errorText = "+" + errorText
		}
		tab.row("%s\t%s%s\t%s\t%s%s\t%s%s\t%s\t%s..%s\t%s..%s", m.Name, bound, m.Format(row.Actual), row.Placement, bound, errorText, bound, strconv.FormatFloat(row.Percentile, 'f', 0, 64), m.Format(m.Range.Median), m.Format(m.Range.P10), m.Format(m.Range.P90), m.Format(m.Range.Min), m.Format(m.Range.Max))
	}
	return tab.close()
}
