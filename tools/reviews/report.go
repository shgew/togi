package main

import (
	"errors"
	"fmt"
	"io"
	"slices"
	"strconv"
	"strings"
	"text/tabwriter"
	"time"
)

// pull is a merged pull request as fetched: its head's review check and robotogi's comments.
type pull struct {
	Number int
	Title  string
	Opened time.Time
	// Check is the conclusion of robotogi's review check on the head commit, empty when the head has none.
	Check    string
	Comments []comment
}

type comment struct {
	URL     string
	Created time.Time
	Body    string
}

type counts struct {
	Priority [len(priorities)]int
	Outcome  [len(outcomes)]int
}

func (c *counts) add(o counts) {
	for i := range c.Priority {
		c.Priority[i] += o.Priority[i]
	}
	for i := range c.Outcome {
		c.Outcome[i] += o.Outcome[i]
	}
}

type row struct {
	Number  int
	Title   string
	Check   string
	Records int
	// FirstRecord is the time from opening to the first review record; meaningful only when Records > 0.
	FirstRecord time.Duration
	Findings    counts
}

type totals struct {
	Pulls      int
	Checked    int
	Successful int
	Recorded   int
	Findings   counts
	// FirstRecords holds each recorded pull request's time to its first record, ascending.
	FirstRecords []time.Duration
}

// summarize builds one row per pull request, ascending by number. A finding repeated by a later record of the same pull request counts once, with the later outcome.
func summarize(pulls []pull) ([]row, error) {
	rows := make([]row, 0, len(pulls))
	for _, p := range pulls {
		r := row{Number: p.Number, Title: p.Title, Check: p.Check}
		comments := slices.SortedStableFunc(slices.Values(p.Comments), func(a, b comment) int { return a.Created.Compare(b.Created) })
		type key struct{ source, priority, text string }
		latest := map[key]string{}
		for _, c := range comments {
			rec, err := parseRecord(c.Body)
			if errors.Is(err, errNoRecord) {
				continue
			}
			if err != nil {
				return nil, fmt.Errorf("parse record %s: %w", c.URL, err)
			}
			if r.Records == 0 {
				r.FirstRecord = c.Created.Sub(p.Opened)
			}
			r.Records++
			for _, f := range rec.Findings {
				latest[key{f.Source, f.Priority, f.Finding}] = f.Outcome.Status
			}
		}
		for k, status := range latest {
			r.Findings.Priority[slices.Index(priorities[:], k.priority)]++
			r.Findings.Outcome[slices.Index(outcomes[:], status)]++
		}
		rows = append(rows, r)
	}
	slices.SortFunc(rows, func(a, b row) int { return a.Number - b.Number })
	return rows, nil
}

func total(rows []row) totals {
	t := totals{Pulls: len(rows)}
	for _, r := range rows {
		if r.Check != "" {
			t.Checked++
		}
		if r.Check == "success" {
			t.Successful++
		}
		if r.Records > 0 {
			t.Recorded++
			t.FirstRecords = append(t.FirstRecords, r.FirstRecord)
		}
		t.Findings.add(r.Findings)
	}
	slices.Sort(t.FirstRecords)
	return t
}

func render(w io.Writer, rows []row, t totals) error {
	tw := tabwriter.NewWriter(w, 0, 0, 2, ' ', 0)
	fmt.Fprintln(tw, "PR\tCHECK\tRECORDS\tP0\tP1\tP2\tP3\tFIXED\tREJECTED\tDEFERRED\tFIRST RECORD\tTITLE")
	for _, r := range rows {
		check, first := r.Check, "-"
		if check == "" {
			check = "none"
		}
		if r.Records > 0 {
			first = formatDuration(r.FirstRecord)
		}
		fmt.Fprintf(tw, "#%d\t%s\t%d\t%s\t%s\t%s\n", r.Number, check, r.Records, countCells(r.Findings), first, r.Title)
	}
	fmt.Fprintf(tw, "total\t%d/%d\t%d/%d\t%s\n", t.Checked, t.Pulls, t.Recorded, t.Pulls, countCells(t.Findings))
	if err := tw.Flush(); err != nil {
		return fmt.Errorf("write table: %w", err)
	}
	f := t.Findings
	fmt.Fprintf(w, "\n%d merged pull requests: %d with robotogi's review check on the head (%d successful), %d with a review record.\n",
		t.Pulls, t.Checked, t.Successful, t.Recorded)
	fmt.Fprintf(w, "Findings: P0 %d, P1 %d, P2 %d, P3 %d; fixed %d, rejected %d, deferred %d.\n",
		f.Priority[0], f.Priority[1], f.Priority[2], f.Priority[3], f.Outcome[0], f.Outcome[1], f.Outcome[2])
	if n := len(t.FirstRecords); n > 0 {
		_, err := fmt.Fprintf(w, "Time from opening to the first record: median %s, longest %s.\n",
			formatDuration(median(t.FirstRecords)), formatDuration(t.FirstRecords[n-1]))
		if err != nil {
			return fmt.Errorf("write totals: %w", err)
		}
	}
	return nil
}

func countCells(c counts) string {
	cells := make([]string, 0, len(c.Priority)+len(c.Outcome))
	for _, n := range c.Priority {
		cells = append(cells, strconv.Itoa(n))
	}
	for _, n := range c.Outcome {
		cells = append(cells, strconv.Itoa(n))
	}
	return strings.Join(cells, "\t")
}

func median(sorted []time.Duration) time.Duration {
	n := len(sorted)
	if n%2 == 1 {
		return sorted[n/2]
	}
	return (sorted[n/2-1] + sorted[n/2]) / 2
}

func formatDuration(d time.Duration) string {
	d = d.Round(time.Minute)
	h, m := int(d/time.Hour), int(d%time.Hour/time.Minute)
	if h == 0 {
		return fmt.Sprintf("%dm", m)
	}
	return fmt.Sprintf("%dh%02dm", h, m)
}
