package main

import (
	"fmt"
	"slices"
	"strings"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func renderHunts(tab *table, p *projection, since time.Time) {
	tab.section("Hunts", "hunt\tstart\thours\tfailing trial\tparked offsets\tcandidates\tplanned/run/inferred/skipped\ttrials\tcrashes\tresult\tmembers/culprit\tcommitment")
	for _, h := range p.hunts {
		if !selected(h.time, since) {
			continue
		}
		ran, inferred, skipped, crashes := 0, 0, 0, 0
		for _, m := range h.groups {
			if len(m.trials) > 0 {
				ran++
			}
			if m.plan.Inferred != "" {
				inferred++
			}
			if m.plan.Skipped {
				skipped++
			}
		}
		for _, t := range h.trials {
			if crash(t) {
				crashes++
			}
		}
		parked := "all-zero"
		if h.start.ParkedSeq != 0 {
			parked = fmt.Sprintf("#%d", h.start.ParkedSeq)
		}
		result, members := "open", "-"
		if h.result != nil {
			result = h.result.Result
			members = coreList(h.result.Cores)
			if len(h.result.Members) > 0 {
				a := slices.Clone(h.result.Members)
				slices.SortFunc(a, func(a, b journal.CombinationMember) int { return a.Core - b.Core })
				labels := make([]string, len(a))
				for i, m := range a {
					labels[i] = fmt.Sprintf("%02d:%d", m.Core, m.Offset)
				}
				members = strings.Join(labels, ",")
			}
		}
		tab.row("%d\t%s\t%.3f\t%s\t%s\t%d\t%d/%d/%d/%d\t%d\t%d\t%s\t%s\t%s", h.start.Hunt, stamp(h.time), h.end.Sub(h.time).Hours(), h.start.Trial, parked, len(h.start.Candidates), len(h.groups), ran, inferred, skipped, len(h.trials), crashes, result, members, h.commitment)
	}
}

func renderChecking(tab *table, p *projection, events []journal.Event, since time.Time) {
	tab.section("Checking", "metric\tcount")
	starts, ends, full := 0, 0, 0
	for _, e := range events {
		if !selected(e.Time, since) {
			continue
		}
		if v, ok := e.Data.(*journal.CheckingLap); ok {
			if v.Event == journal.LapStart {
				starts++
			}
			if v.Event == journal.LapEnd {
				ends++
				if v.Full {
					full++
				}
			}
		}
	}
	tab.row("laps started\t%d", starts)
	tab.row("laps ended\t%d", ends)
	tab.row("full laps\t%d", full)
	reruns, failed := 0, 0
	seen := map[int]bool{}
	for _, t := range p.trials {
		if !t.intent.Rerun {
			continue
		}
		k := t.seq
		if len(t.cause) > 0 {
			k = t.cause[0]
		}
		if seen[k] {
			continue
		}
		seen[k] = true
		if selected(t.time, since) {
			reruns++
			if t.end != nil && t.end.Outcome == journal.OutcomeFailure {
				failed++
			}
		}
	}
	tab.row("reruns\t%d", reruns)
	tab.row("reruns failing first start\t%d", failed)
	tab.section("Checking steps and together outcomes", "lap\tregime\tloaded cores\toutcome\ttrials")
	counts := map[string]int{}
	for _, t := range p.trials {
		if selected(t.time, since) && t.intent.Phase == journal.PhaseChecking && t.intent.Condition == machine.Together {
			counts[fmt.Sprintf("%04d\t%s\t%s\t%s", t.intent.Lap, t.intent.Regime, coreList(loaded(t.intent, p.cores)), outcome(t))]++
		}
	}
	for _, k := range keys(counts) {
		tab.row("%s\t%d", k, counts[k])
	}
}

type reuseCount struct{ groups, established, passed, failed, trials, seconds int }

func renderEvidence(tab *table, p *projection, events []journal.Event, since time.Time) {
	tab.section("Evidence quality", "warning\tcount")
	warnings := map[string]int{}
	for _, e := range events {
		if selected(e.Time, since) {
			if v, ok := e.Data.(*journal.TunerWarning); ok {
				warnings[v.Warning]++
			}
		}
	}
	for _, k := range keys(warnings) {
		tab.row("%s\t%d", k, warnings[k])
	}
	tab.row("decisions resting on a single carried failure\t%d", singleCarriedFailureDecisions(events, since))
	tab.section("Failures after prior passes", "trial class\tfailures with prior passes")
	contradictions := map[string]int{}
	for _, t := range p.trials {
		if selected(t.time, since) && t.end != nil && t.end.Outcome == journal.OutcomeFailure && priorPasses(p.trials, t.intent, t.seq, p.cores, p.idle) > 0 {
			contradictions[class(t.intent, p.cores)]++
		}
	}
	for _, k := range keys(contradictions) {
		tab.row("%s\t%d", k, contradictions[k])
	}
	tab.section("Prior evidence per hunt group", "hunt\tgroup\tstage\tprior passes\trequired\testablished\tpasses again\tfailures again\ttrials\thours")
	summary := map[string]*reuseCount{}
	for _, h := range p.hunts {
		if !selected(h.time, since) {
			continue
		}
		for _, m := range h.groups {
			if len(m.trials) == 0 {
				continue
			}
			prior := priorPasses(p.trials, m.trials[0].intent, h.seq, p.cores, p.idle)
			established := prior >= h.start.Starts
			passes, failures, cost := 0, 0, 0
			for _, t := range m.trials {
				cost += seconds(t)
				if t.end != nil {
					switch t.end.Outcome {
					case journal.OutcomePass:
						passes++
					case journal.OutcomeFailure:
						failures++
					case journal.OutcomeInconclusive:
					}
				}
			}
			tab.row("%d\t%d\t%s\t%d\t%d\t%t\t%d\t%d\t%d\t%.3f", h.start.Hunt, m.plan.Group, m.plan.Stage, prior, h.start.Starts, established, passes, failures, len(m.trials), float64(cost)/3600)
			k := fmt.Sprintf("%04d\t%s", h.start.Hunt, m.plan.Stage)
			s := summary[k]
			if s == nil {
				s = &reuseCount{}
				summary[k] = s
			}
			s.groups++
			if established {
				s.established++
				if failures > 0 {
					s.failed++
				} else if passes >= h.start.Starts {
					s.passed++
				}
				s.trials += len(m.trials)
				s.seconds += cost
			}
		}
	}
	tab.section("Prior evidence by hunt and stage", "hunt\tstage\tran groups\testablished groups\tpassed again\tfailed again\tredundant trials\tredundant hours")
	for _, k := range keys(summary) {
		s := summary[k]
		tab.row("%s\t%d\t%d\t%d\t%d\t%d\t%.3f", k, s.groups, s.established, s.passed, s.failed, s.trials, float64(s.seconds)/3600)
	}
}

func singleCarriedFailureDecisions(events []journal.Event, since time.Time) int {
	bySeq := make(map[int]journal.Event, len(events))
	for _, e := range events {
		bySeq[e.Seq] = e
	}
	count := 0
	for _, e := range events {
		if !selected(e.Time, since) {
			continue
		}
		switch p := e.Data.(type) {
		case *journal.TunerDecision:
			if p.Decision != journal.Backoff {
				continue
			}
		case *journal.HuntGroup:
			if p.Inferred != "failure" {
				continue
			}
		default:
			continue
		}
		n, carried := 0, false
		seen := make(map[int]bool)
		pending := slices.Clone(e.Cause)
		for len(pending) > 0 {
			seq := pending[len(pending)-1]
			pending = pending[:len(pending)-1]
			if seen[seq] {
				continue
			}
			seen[seq] = true
			source, ok := bySeq[seq]
			if !ok {
				continue
			}
			switch p := source.Data.(type) {
			case *journal.TrialCarried:
				if p.Outcome == journal.OutcomeFailure {
					n++
					carried = true
				}
			case *journal.FailureCarried:
				n++
				carried = true
			case *journal.Failure:
				n++
				carried = false
			case *journal.TrialEnd:
				if p.Outcome == journal.OutcomeFailure {
					n++
					carried = false
				}
			default:
				pending = append(pending, source.Cause...)
			}
		}
		if n == 1 && carried {
			count++
		}
	}
	return count
}

type depthCount struct{ starts, failures int }

func renderDepth(tab *table, p *projection, since time.Time) {
	tab.section("Failure rate by depth", "regime / workload / loaded cores / duration\tshallowest offset\tstarts\tfailures\trate")
	groups := map[string]map[int]*depthCount{}
	failing := map[string]bool{}
	for _, t := range p.trials {
		if !selected(t.time, since) || !t.started {
			continue
		}
		cores := loaded(t.intent, p.cores)
		if len(cores) < 2 {
			continue
		}
		depth := -51
		valid := true
		for _, c := range cores {
			if c < 0 || c >= len(t.intent.Profile) {
				valid = false
				break
			}
			depth = max(depth, t.intent.Profile[c])
		}
		if !valid {
			continue
		}
		k := class(t.intent, p.cores)
		if groups[k] == nil {
			groups[k] = map[int]*depthCount{}
		}
		if groups[k][depth] == nil {
			groups[k][depth] = &depthCount{}
		}
		d := groups[k][depth]
		d.starts++
		if t.end != nil && t.end.Outcome == journal.OutcomeFailure {
			d.failures++
			failing[k] = true
		}
	}
	for _, k := range keys(groups) {
		if !failing[k] {
			continue
		}
		depths := make([]int, 0, len(groups[k]))
		for d := range groups[k] {
			depths = append(depths, d)
		}
		slices.Sort(depths)
		for _, d := range depths {
			c := groups[k][d]
			tab.row("%s\t%d\t%d\t%d\t%.1f%%", k, d, c.starts, c.failures, 100*float64(c.failures)/float64(c.starts))
		}
	}
}
