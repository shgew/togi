package main

import (
	"fmt"
	"math"
	"time"

	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/requests"
)

type r7Measurement struct {
	key      string
	failed   bool
	requests map[int]float64
	top      []int
}

// r7Measurements prefers a trial end's request fields. A decisive end without
// them uses the summary facts recovered from the trial's persisted samples.
// A carried fact counts once per source and only with both request fields: the
// missing-telemetry count is about this journal's own trials, which a carried
// fact of this session would repeat.
func r7Measurements(session facts.Session, p *projection, since time.Time) []r7Measurement {
	recovered := map[int]facts.Fact{}
	for _, f := range session.Facts {
		if f.Kind == facts.TrialFact && f.Session == session.ID {
			recovered[f.Seq] = f
		}
	}
	var out []r7Measurement
	for _, t := range p.trials {
		if t.Intent.Regime != machine.R7 || !selected(t.Time, since) || !t.Started || t.End == nil {
			continue
		}
		m := r7Measurement{key: t.key, failed: t.End.Outcome == journal.OutcomeFailure, requests: t.End.VoltageRequestsV, top: t.End.TopRequesters}
		if f, ok := recovered[t.EndSeq]; ok && (len(m.requests) == 0 || len(m.top) == 0) {
			m.requests, m.top = f.VoltageRequestsV, f.TopRequesters
		}
		out = append(out, m)
	}
	type source struct {
		session string
		seq     int
	}
	seen := map[source]bool{}
	for _, f := range session.Carried {
		at := source{f.Session, f.Seq}
		if f.Kind != facts.TrialFact || f.Class.Regime != machine.R7 || len(f.VoltageRequestsV) == 0 || len(f.TopRequesters) == 0 || f.Session == session.ID || seen[at] || !selected(f.Time, since) {
			continue
		}
		seen[at] = true
		key := class(&journal.TrialIntent{Regime: f.Class.Regime, Workload: f.Class.Workload, Cores: f.Class.Cores, DurationS: f.Class.DurationS}, p.cores)
		out = append(out, r7Measurement{key: key, failed: f.Outcome == journal.OutcomeFailure, requests: f.VoltageRequestsV, top: f.TopRequesters})
	}
	return out
}

func renderRequests(tab *table, measurements []r7Measurement) {
	type counts struct{ trials, failures int }
	bins, requesters := map[string]counts{}, map[string]counts{}
	missingFailures := 0
	for _, m := range measurements {
		top, ok := requests.Top(m.requests)
		if !ok || len(m.top) == 0 {
			if m.failed {
				missingFailures++
			}
			continue
		}
		add := func(counted map[string]counts, key string) {
			x := counted[key]
			x.trials++
			if m.failed {
				x.failures++
			}
			counted[key] = x
		}
		low := math.Floor(top*250+1e-9) / 250
		add(bins, fmt.Sprintf("%s\t[%.3f, %.3f)", m.key, low, low+0.004))
		for _, core := range m.top {
			add(requesters, fmt.Sprintf("%s\t%02d", m.key, core))
		}
	}
	tab.section("R7 voltage requests", "field\tcount")
	tab.row("failures without request telemetry\t%d", missingFailures)
	tab.section("R7 trials by top request (4 mV bins)", "class\ttop request V\ttrials\tfailures")
	if len(bins) == 0 {
		tab.row("none")
	}
	for _, key := range keys(bins) {
		x := bins[key]
		tab.row("%s\t%d\t%d", key, x.trials, x.failures)
	}
	tab.section("R7 trials by top requester", "class\tcore\ttrials\tfailures")
	if len(requesters) == 0 {
		tab.row("none")
	}
	for _, key := range keys(requesters) {
		x := requesters[key]
		tab.row("%s\t%d\t%d", key, x.trials, x.failures)
	}
}
