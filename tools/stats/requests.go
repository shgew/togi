package main

import (
	"cmp"
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

// requestBin is a class's top voltage request rounded down to its 4 mV bin.
type requestBin struct {
	class string
	low   float64
}

type requester struct {
	class string
	core  int
}

type requestCounts struct{ trials, failures int }

type requestMetrics struct {
	missingFailures int
	bins            []entry[requestBin, requestCounts]
	requesters      []entry[requester, requestCounts]
}

func computeRequests(measurements []r7Measurement) requestMetrics {
	bins, requesters := map[requestBin]requestCounts{}, map[requester]requestCounts{}
	missingFailures := 0
	for _, m := range measurements {
		top, ok := requests.Top(m.requests)
		if !ok || len(m.top) == 0 {
			if m.failed {
				missingFailures++
			}
			continue
		}
		count := func(x requestCounts) requestCounts {
			x.trials++
			if m.failed {
				x.failures++
			}
			return x
		}
		bin := requestBin{m.key, math.Floor(top*250+1e-9) / 250}
		bins[bin] = count(bins[bin])
		for _, core := range m.top {
			r := requester{m.key, core}
			requesters[r] = count(requesters[r])
		}
	}
	return requestMetrics{
		missingFailures: missingFailures,
		bins: sorted(bins, func(a, b requestBin) int {
			return cmp.Or(cmp.Compare(a.class, b.class), cmp.Compare(a.low, b.low))
		}),
		requesters: sorted(requesters, func(a, b requester) int {
			return cmp.Or(cmp.Compare(a.class, b.class), cmp.Compare(a.core, b.core))
		}),
	}
}

func renderRequests(tab *table, m requestMetrics) {
	tab.section("R7 voltage requests", "field\tcount")
	tab.row("failures without request telemetry\t%d", m.missingFailures)
	tab.section("R7 trials by top request (4 mV bins)", "class\ttop request V\ttrials\tfailures")
	if len(m.bins) == 0 {
		tab.row("none")
	}
	for _, x := range m.bins {
		tab.row("%s\t[%.3f, %.3f)\t%d\t%d", x.key.class, x.key.low, x.key.low+0.004, x.value.trials, x.value.failures)
	}
	tab.section("R7 trials by top requester", "class\tcore\ttrials\tfailures")
	if len(m.requesters) == 0 {
		tab.row("none")
	}
	for _, x := range m.requesters {
		tab.row("%s\t%02d\t%d\t%d", x.key.class, x.key.core, x.value.trials, x.value.failures)
	}
}
