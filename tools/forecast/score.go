package forecast

import (
	"fmt"
	"slices"
	"strings"

	"github.com/shgew/togi/internal/journal"
)

// Placement of a real value against a metric's 10th to 90th percentile.
const (
	Inside = "yes"
	Below  = "below"
	Above  = "above"
	// Open: a lower bound inside or below the range; the run had not finished, so the final value may still land
	// anywhere at or above it.
	Open = "open"
	// Unforecast: the ensemble has no value for the metric.
	Unforecast = "-"
)

// Row scores one metric.
type Row struct {
	Metric Metric
	Actual float64
	// Bound marks an actual value that only bounds the final one from below: the run had not concluded.
	Bound      bool
	Placement  string
	Error      float64
	Percentile float64
}

// Score is a real run scored against a forecast.
type Score struct {
	Record  Record
	Outcome Outcome
	// Builds lists the distinct builds that recorded events after the anchor, in order.
	Builds []journal.Build
	// Flags lists differences that make the forecast less comparable: another build revision or ruleset, or a
	// forecast made from a dirty tree.
	Flags []string
	Rows  []Row
}

// Score scores the real run in sessions against the forecast. It refuses sessions that lack the anchor or changed
// the journal through it.
func (r Record) Score(sessions []Session) (Score, error) {
	outcome, err := After(sessions, r.Anchor)
	if err != nil {
		return Score{}, err
	}
	metrics, err := Metrics(r.Runs)
	if err != nil {
		return Score{}, err
	}
	if cores := len(metrics) - 4; len(outcome.Profile) != cores {
		return Score{}, fmt.Errorf("run has %d cores, forecast %d", len(outcome.Profile), cores)
	}
	s := Score{Record: r, Outcome: outcome, Builds: buildsAfter(sessions, r.Anchor)}
	if r.Dirty {
		s.Flags = append(s.Flags, fmt.Sprintf("the forecast was made from a dirty tree at commit %s", r.Commit))
	}
	for _, b := range s.Builds {
		if !sameRevision(b.Rev, r.Commit) {
			s.Flags = append(s.Flags, fmt.Sprintf("build revision %s ran after the anchor; the forecast is at commit %s", b.Rev, r.Commit))
		}
		if b.Ruleset != r.Ruleset {
			s.Flags = append(s.Flags, fmt.Sprintf("ruleset %d ran after the anchor; the forecast used ruleset %d", b.Ruleset, r.Ruleset))
		}
	}
	bound := outcome.Status == Censored
	actuals := make([]float64, 0, len(metrics))
	for _, offset := range outcome.Profile {
		actuals = append(actuals, float64(offset))
	}
	actuals = append(actuals, float64(outcome.Depth), outcome.Hours, float64(outcome.Crashes), float64(outcome.Hunts))
	for i, m := range metrics {
		row := Row{Metric: m, Actual: actuals[i], Bound: bound && i >= len(metrics)-3}
		switch {
		case m.Range == nil:
			row.Placement = Unforecast
		case row.Actual > m.Range.P90:
			row.Placement = Above
		case row.Bound:
			row.Placement = Open
		case row.Actual < m.Range.P10:
			row.Placement = Below
		default:
			row.Placement = Inside
		}
		if m.Range != nil {
			row.Error = row.Actual - m.Range.Median
			row.Percentile = m.Percentile(row.Actual, row.Bound)
		}
		s.Rows = append(s.Rows, row)
	}
	return s, nil
}

// buildsAfter lists each distinct build stamped by session.start or config.loaded after the anchor.
func buildsAfter(sessions []Session, a Anchor) []journal.Build {
	var builds []journal.Build
	add := func(b journal.Build) {
		b = journal.Build{Version: b.Version, Rev: b.Rev, Ruleset: b.Ruleset}
		if !slices.Contains(builds, b) {
			builds = append(builds, b)
		}
	}
	after := false
	for _, s := range sessions {
		ruleset := journal.BuildOf(s.Events).Ruleset
		for _, e := range s.Events {
			if !after {
				after = s.ID == a.Session && e.Seq == a.Seq
				continue
			}
			switch p := e.Data.(type) {
			case *journal.SessionStart:
				add(journal.Build{Version: p.Version, Rev: p.Rev, Ruleset: ruleset})
			case *journal.ConfigLoaded:
				if p.Version != "" {
					add(journal.Build{Version: p.Version, Rev: p.Rev, Ruleset: ruleset})
				}
			}
		}
	}
	return builds
}

// sameRevision matches a build's short revision against a forecast commit, either of which may be abbreviated.
func sameRevision(rev, commit string) bool {
	if rev == "" || commit == "" || strings.HasSuffix(rev, "-dirty") {
		return false
	}
	return strings.HasPrefix(rev, commit) || strings.HasPrefix(commit, rev)
}
