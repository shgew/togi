package main

import (
	"fmt"
	"math"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/requests"
)

func renderRequests(tab *table, p *projection, since time.Time) {
	type counts struct{ trials, failures int }
	bins, requesters := map[string]counts{}, map[string]counts{}
	missingFailures := 0
	for _, t := range p.trials {
		if t.intent.Regime != machine.R7 || !selected(t.time, since) || !t.started || t.end == nil {
			continue
		}
		failed := t.end.Outcome == journal.OutcomeFailure
		top, ok := requests.Top(t.end.VoltageRequestsV)
		if !ok || len(t.end.TopRequesters) == 0 {
			if failed {
				missingFailures++
			}
			continue
		}
		add := func(m map[string]counts, key string) {
			x := m[key]
			x.trials++
			if failed {
				x.failures++
			}
			m[key] = x
		}
		low := math.Floor(top*250+1e-9) / 250
		add(bins, fmt.Sprintf("%s\t[%.3f, %.3f)", t.key, low, low+0.004))
		for _, core := range t.end.TopRequesters {
			add(requesters, fmt.Sprintf("%s\t%02d", t.key, core))
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
