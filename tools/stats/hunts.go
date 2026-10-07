package main

import (
	"cmp"
	"fmt"
	"slices"
	"strings"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type huntMetrics struct {
	*huntInfo
	ran, inferred, skipped, crashes int
}

func computeHunts(p *projection, since time.Time) []huntMetrics {
	var hunts []huntMetrics
	for _, h := range p.hunts {
		if !selected(h.time, since) {
			continue
		}
		m := huntMetrics{huntInfo: h}
		for _, g := range h.groups {
			if len(g.trials) > 0 {
				m.ran++
			}
			if g.plan.Inferred != "" {
				m.inferred++
			}
			if g.plan.Skipped {
				m.skipped++
			}
		}
		for _, t := range h.trials {
			if crash(t) {
				m.crashes++
			}
		}
		hunts = append(hunts, m)
	}
	return hunts
}

func renderHunts(tab *table, hunts []huntMetrics) {
	tab.section("Hunts", "hunt\tstart\thours\tfailing trial\tparked offsets\tcandidates\tplanned/run/inferred/skipped\ttrials\tcrashes\tresult\tmembers/culprit\tcommitment")
	for _, h := range hunts {
		parked := "all-zero"
		switch {
		case h.start.ParkedSeq != 0:
			parked = fmt.Sprintf("#%d", h.start.ParkedSeq)
		case slices.ContainsFunc(h.start.Parked, func(offset int) bool { return offset != 0 }):
			// A located hunt holds its loaded cores at their failing offsets and parks the rest at 0.
			parked = "loaded-held"
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
		tab.row("%d\t%s\t%.3f\t%s\t%s\t%d\t%d/%d/%d/%d\t%d\t%d\t%s\t%s\t%s", h.start.Hunt, stamp(h.time), h.end.Sub(h.time).Hours(), h.start.Trial, parked, len(h.start.Candidates), len(h.groups), h.ran, h.inferred, h.skipped, len(h.trials), h.crashes, result, members, h.commitment)
	}
}

// stepKey is a checking step: cycle is zero for reruns, which belong to no cycle.
type stepKey struct {
	rerun   bool
	cycle   int
	regime  machine.Regime
	cores   string
	outcome string
}

type checkingMetrics struct {
	cyclesStarted, cyclesEnded, fullCycles int
	reruns, rerunsFailing                  int
	steps                                  []entry[stepKey, int]
}

func computeChecking(p *projection, events []journal.Event, since time.Time) checkingMetrics {
	var m checkingMetrics
	for _, e := range events {
		if !selected(e.Time, since) {
			continue
		}
		if v, ok := e.Data.(*journal.CheckingCycle); ok {
			if v.Event == journal.CycleStart {
				m.cyclesStarted++
			}
			if v.Event == journal.CycleEnd {
				m.cyclesEnded++
				if v.Full {
					m.fullCycles++
				}
			}
		}
	}
	seen := map[int]bool{}
	for _, t := range p.trials {
		if !t.Intent.Rerun {
			continue
		}
		k := t.Seq
		if len(t.Cause) > 0 {
			k = t.Cause[0]
		}
		if seen[k] {
			continue
		}
		seen[k] = true
		if selected(t.Time, since) {
			m.reruns++
			if t.End != nil && t.End.Outcome == journal.OutcomeFailure {
				m.rerunsFailing++
			}
		}
	}
	counts := map[stepKey]int{}
	for _, t := range p.trials {
		if selected(t.Time, since) && t.Intent.Phase == journal.PhaseChecking && t.Intent.Condition == machine.Together {
			k := stepKey{rerun: t.Intent.Rerun, regime: t.Intent.Regime, cores: coreList(loaded(t.Intent, p.cores)), outcome: outcome(t)}
			if !k.rerun {
				k.cycle = t.Intent.Cycle
			}
			counts[k]++
		}
	}
	m.steps = sorted(counts, func(a, b stepKey) int {
		return cmp.Or(compareBool(a.rerun, b.rerun), cmp.Compare(a.cycle, b.cycle), cmp.Compare(a.regime, b.regime), cmp.Compare(a.cores, b.cores), cmp.Compare(a.outcome, b.outcome))
	})
	return m
}

func compareBool(a, b bool) int {
	switch {
	case a == b:
		return 0
	case a:
		return 1
	default:
		return -1
	}
}

func renderChecking(tab *table, m checkingMetrics) {
	tab.section("Checking", "metric\tcount")
	tab.row("cycles started\t%d", m.cyclesStarted)
	tab.row("cycles ended\t%d", m.cyclesEnded)
	tab.row("full cycles\t%d", m.fullCycles)
	tab.row("reruns\t%d", m.reruns)
	tab.row("reruns failing first trial\t%d", m.rerunsFailing)
	tab.section("Checking steps and together outcomes", "cycle\tregime\tloaded cores\toutcome\ttrials")
	for _, x := range m.steps {
		k := x.key
		cycle := fmt.Sprintf("%04d", k.cycle)
		if k.rerun {
			cycle = "rerun"
		}
		tab.row("%s\t%s\t%s\t%s\t%d", cycle, k.regime, k.cores, k.outcome, x.value)
	}
}

// groupReuse is one hunt group's prior evidence and what running it found.
type groupReuse struct {
	hunt, group      int
	stage            string
	prior, required  int
	established      bool
	passes, failures int
	trials, seconds  int
}

type stageKey struct {
	hunt  int
	stage string
}

type stageReuse struct{ groups, established, passed, failed, trials, seconds int }

type evidenceMetrics struct {
	warnings       []entry[string, int]
	singleCarried  int
	contradictions []entry[string, int]
	groups         []groupReuse
	stages         []entry[stageKey, stageReuse]
}

func computeEvidence(p *projection, events []journal.Event, since time.Time) evidenceMetrics {
	warnings := map[string]int{}
	for _, e := range events {
		if selected(e.Time, since) {
			if v, ok := e.Data.(*journal.TunerWarning); ok {
				warnings[v.Warning]++
			}
		}
	}
	contradictions := map[string]int{}
	for _, t := range p.trials {
		if selected(t.Time, since) && t.End != nil && t.End.Outcome == journal.OutcomeFailure && priorPasses(p.trials, t.Intent, t.Seq, p.cores, p.idle) > 0 {
			contradictions[class(t.Intent, p.cores)]++
		}
	}
	var groups []groupReuse
	stages := map[stageKey]stageReuse{}
	for _, h := range p.hunts {
		if !selected(h.time, since) {
			continue
		}
		for _, m := range h.groups {
			if len(m.trials) == 0 {
				continue
			}
			g := groupReuse{hunt: h.start.Hunt, group: m.plan.Group, stage: m.plan.Stage, required: h.start.Trials, trials: len(m.trials)}
			g.prior = priorPasses(p.trials, m.trials[0].Intent, h.seq, p.cores, p.idle)
			g.established = g.prior >= g.required
			for _, t := range m.trials {
				g.seconds += seconds(t)
				if t.End != nil {
					switch t.End.Outcome {
					case journal.OutcomePass:
						g.passes++
					case journal.OutcomeFailure:
						g.failures++
					case journal.OutcomeInconclusive:
					}
				}
			}
			groups = append(groups, g)
			k := stageKey{g.hunt, g.stage}
			s := stages[k]
			s.groups++
			if g.established {
				s.established++
				if g.failures > 0 {
					s.failed++
				} else if g.passes >= g.required {
					s.passed++
				}
				s.trials += g.trials
				s.seconds += g.seconds
			}
			stages[k] = s
		}
	}
	return evidenceMetrics{
		warnings:       sorted(warnings, strings.Compare),
		singleCarried:  singleCarriedFailureDecisions(events, since),
		contradictions: sorted(contradictions, strings.Compare),
		groups:         groups,
		stages: sorted(stages, func(a, b stageKey) int {
			return cmp.Or(cmp.Compare(a.hunt, b.hunt), cmp.Compare(a.stage, b.stage))
		}),
	}
}

func renderEvidence(tab *table, m evidenceMetrics) {
	tab.section("Evidence quality", "warning\tcount")
	for _, x := range m.warnings {
		tab.row("%s\t%d", x.key, x.value)
	}
	tab.row("decisions resting on a single carried failure\t%d", m.singleCarried)
	tab.section("Failures after prior passes", "trial class\tfailures with prior passes")
	for _, x := range m.contradictions {
		tab.row("%s\t%d", x.key, x.value)
	}
	tab.section("Prior evidence per hunt group", "hunt\tgroup\tstage\tprior passes\trequired\testablished\tpasses again\tfailures again\ttrials\thours")
	for _, g := range m.groups {
		tab.row("%d\t%d\t%s\t%d\t%d\t%t\t%d\t%d\t%d\t%.3f", g.hunt, g.group, g.stage, g.prior, g.required, g.established, g.passes, g.failures, g.trials, float64(g.seconds)/3600)
	}
	tab.section("Prior evidence by hunt and stage", "hunt\tstage\tran groups\testablished groups\tpassed again\tfailed again\tredundant trials\tredundant hours")
	for _, x := range m.stages {
		s := x.value
		tab.row("%04d\t%s\t%d\t%d\t%d\t%d\t%d\t%.3f", x.key.hunt, x.key.stage, s.groups, s.established, s.passed, s.failed, s.trials, float64(s.seconds)/3600)
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

// depthRate counts a multi-core class's trials by the shallowest loaded offset.
type depthRate struct {
	class                   string
	depth, trials, failures int
}

func computeDepth(p *projection, since time.Time) []depthRate {
	type depthCount struct{ trials, failures int }
	groups := map[string]map[int]*depthCount{}
	failing := map[string]bool{}
	for _, t := range p.trials {
		if !selected(t.Time, since) || !t.Started {
			continue
		}
		cores := loaded(t.Intent, p.cores)
		if len(cores) < 2 {
			continue
		}
		depth := -51
		valid := true
		for _, c := range cores {
			if c < 0 || c >= len(t.Intent.Profile) {
				valid = false
				break
			}
			depth = max(depth, t.Intent.Profile[c])
		}
		if !valid {
			continue
		}
		k := class(t.Intent, p.cores)
		if groups[k] == nil {
			groups[k] = map[int]*depthCount{}
		}
		if groups[k][depth] == nil {
			groups[k][depth] = &depthCount{}
		}
		d := groups[k][depth]
		d.trials++
		if t.End != nil && t.End.Outcome == journal.OutcomeFailure {
			d.failures++
			failing[k] = true
		}
	}
	var rates []depthRate
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
			rates = append(rates, depthRate{k, d, c.trials, c.failures})
		}
	}
	return rates
}

func renderDepth(tab *table, rates []depthRate) {
	tab.section("Failure rate by depth", "regime / workload / loaded cores / duration\tshallowest offset\ttrials\tfailures\trate")
	for _, r := range rates {
		tab.row("%s\t%d\t%d\t%d\t%.1f%%", r.class, r.depth, r.trials, r.failures, 100*float64(r.failures)/float64(r.trials))
	}
}
