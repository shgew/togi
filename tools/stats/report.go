package main

import (
	"fmt"
	"io"
	"slices"
	"strconv"
	"strings"
	"text/tabwriter"
	"time"

	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type table struct {
	out    io.Writer
	writer *tabwriter.Writer
}

func (t *table) section(name, header string) {
	if t.writer != nil {
		_ = t.writer.Flush()
		fmt.Fprintln(t.out)
	}
	fmt.Fprintln(t.out, name)
	t.writer = tabwriter.NewWriter(t.out, 0, 4, 2, ' ', 0)
	fmt.Fprintln(t.writer, header)
}
func (t *table) row(format string, args ...any) { fmt.Fprintf(t.writer, format+"\n", args...) }
func keys[V any](m map[string]V) []string {
	k := make([]string, 0, len(m))
	for s := range m {
		k = append(k, s)
	}
	slices.Sort(k)
	return k
}
func stamp(t time.Time) string                   { return t.UTC().Format(time.RFC3339) }
func selected(t time.Time, since time.Time) bool { return !t.Before(since) }
func seconds(t *trial) int {
	if t.end == nil {
		return 0
	}
	return t.end.DurationS
}
func outcome(t *trial) string {
	if t.end == nil {
		return "open"
	}
	return string(t.end.Outcome)
}
func crash(t *trial) bool {
	return t.crashed || t.end != nil && t.end.Outcome == journal.OutcomeFailure && t.end.Signal == machine.Crash
}

func report(out io.Writer, session facts.Session, since time.Time) error {
	events := session.Events
	p := project(session)
	tab := &table{out: out}
	renderSession(tab, p, events, since)
	gaps := renderTime(tab, p, since)
	renderFailures(tab, p, events, since, gaps)
	renderHunts(tab, p, since)
	renderChecking(tab, p, events, since)
	renderEvidence(tab, p, events, since)
	renderDepth(tab, p, since)
	renderRequests(tab, p, since)
	return renderOutcomes(tab, p, since)
}

func renderSession(tab *table, p *projection, events []journal.Event, since time.Time) {
	tab.section("Session", "field\tvalue")
	rules := map[string]int{}
	builds := map[string]int{}
	for _, e := range events {
		switch v := e.Data.(type) {
		case *journal.SessionStart:
			tab.row("id\t%s", v.Session)
			rules[strconv.Itoa(max(v.Ruleset, 1))]++
		case *journal.ConfigLoaded:
			builds[buildName(v.Build)]++
			if v.Ruleset != 0 {
				rules[strconv.Itoa(v.Ruleset)]++
			}
		}
	}
	tab.row("rulesets\t%s", strings.Join(keys(rules), ", "))
	tab.row("builds\t%s", strings.Join(keys(builds), ", "))
	if len(events) > 0 {
		tab.row("first\t%s", stamp(events[0].Time))
		tab.row("last\t%s", stamp(events[len(events)-1].Time))
	}
	if !since.IsZero() {
		tab.row("since\t%s", stamp(since))
	}
	tab.section("Runs", "run\tstart\tend\tbuild\ttrials\tcrashes\tended")
	for i, r := range p.runs {
		if selected(r.end, since) {
			tab.row("%d\t%s\t%s\t%s\t%d\t%d\t%s", i+1, stamp(r.start), stamp(r.end), strings.Join(keys(r.builds), ","), r.trials, r.crashes, r.ending)
		}
	}
}

func renderTime(tab *table, p *projection, since time.Time) []float64 {
	tab.section("Time", "phase / condition / regime / outcome\ttrial seconds\thours")
	times := map[string]int{}
	for _, t := range p.trials {
		if selected(t.time, since) {
			k := fmt.Sprintf("%s / %s / %s / %s", t.intent.Phase, t.intent.Condition, t.intent.Regime, outcome(t))
			times[k] += seconds(t)
		}
	}
	for _, k := range keys(times) {
		tab.row("%s\t%d\t%.3f", k, times[k], float64(times[k])/3600)
	}
	downtime := 0.0
	gaps := []float64{}
	for _, t := range p.trials {
		if !selected(t.time, since) || !crash(t) {
			continue
		}
		if next, ok := p.nextBoot[t.boot]; ok {
			boot := p.boots[next]
			gaps = append(gaps, boot.firstEvent.Sub(t.lastEvidence).Seconds())
			downtime += boot.start.Sub(t.lastEvidence).Seconds()
		}
	}
	tab.row("crash downtime (last evidence to next boot)\t%.3f\t%.3f", downtime, downtime/3600)
	return gaps
}

func renderFailures(tab *table, p *projection, events []journal.Event, since time.Time, gaps []float64) {
	tab.section("Failures", "regime\tworkload\tsignal\tattribution\tcount")
	failures := map[string]int{}
	ccds := map[int]int{}
	for _, c := range p.cores {
		ccds[c.Core] = c.CCD
	}
	loadedCCDs := map[string]int{}
	resets := map[string]int{}
	histogram := make([]int, 5)
	for _, e := range events {
		if !selected(e.Time, since) {
			continue
		}
		switch v := e.Data.(type) {
		case *journal.Failure:
			if v.KnownFailure != 0 {
				continue
			}
			workload := "-"
			regime := v.Regime
			tr := p.byID[v.Trial]
			if tr != nil {
				workload = tr.intent.Workload
				if regime == "" {
					regime = tr.intent.Regime
				}
			}
			failures[fmt.Sprintf("%s\t%s\t%s\t%s", regime, workload, v.Signal, v.Attribution)]++
			if v.Signal == machine.Crash && v.Attribution == journal.Unattributed {
				loadedCCDs[loadedCCD(tr, p.cores, ccds)]++
			}
		case *journal.CrashDetected:
			reason := string(v.ResetReason)
			if reason == "" {
				reason = "unknown"
			}
			resets[reason]++
		}
	}
	for _, k := range keys(failures) {
		tab.row("%s\t%d", k, failures[k])
	}
	renderExposure(tab, p, since)
	for _, t := range p.trials {
		if selected(t.time, since) && crash(t) {
			d := seconds(t)
			b := 4
			switch {
			case d == 0:
				b = 0
			case d < 30:
				b = 1
			case d < 60:
				b = 2
			case d < 120:
				b = 3
			}
			histogram[b]++
		}
	}
	tab.section("Unattributed crashes by loaded CCD", "CCD(s)\tcount")
	for _, k := range keys(loadedCCDs) {
		tab.row("%s\t%d", k, loadedCCDs[k])
	}
	tab.section("Crash timing (last evidence)", "seconds\tcount")
	for i, k := range []string{"0", "1-29", "30-59", "60-119", "120+"} {
		tab.row("%s\t%d", k, histogram[i])
	}
	tab.section("Reset reasons", "reason\tcount")
	for _, k := range keys(resets) {
		tab.row("%s\t%d", k, resets[k])
	}
	tab.section("Recovery gap", "count\tmin seconds\tmedian seconds\tmax seconds")
	if len(gaps) > 0 {
		slices.Sort(gaps)
		median := gaps[len(gaps)/2]
		if len(gaps)%2 == 0 {
			median = (gaps[len(gaps)/2-1] + median) / 2
		}
		tab.row("%d\t%.3f\t%.3f\t%.3f", len(gaps), gaps[0], median, gaps[len(gaps)-1])
	} else {
		tab.row("0\t-\t-\t-")
	}
}

func renderOutcomes(tab *table, p *projection, since time.Time) error {
	tab.section("Inconclusive trials", "trial\tcondition\treason")
	for _, t := range p.trials {
		if selected(t.time, since) && t.end != nil && t.end.Outcome == journal.OutcomeInconclusive {
			tab.row("%s\t%s\t%s", t.intent.Trial, t.intent.Condition, t.end.Reason)
		}
	}
	tab.section("Tctl", "regime\tmax C\tpassing trials")
	temps := map[string]int{}
	for _, t := range p.trials {
		if selected(t.time, since) && t.end != nil && t.end.Outcome == journal.OutcomePass && t.end.TctlMaxC != nil {
			temps[fmt.Sprintf("%s\t%03d", t.intent.Regime, *t.end.TctlMaxC)]++
		}
	}
	for _, k := range keys(temps) {
		regime, temp, _ := strings.Cut(k, "\t")
		n, _ := strconv.Atoi(temp)
		tab.row("%s\t%d\t%d", regime, n, temps[k])
	}
	return tab.writer.Flush()
}

func renderExposure(tab *table, p *projection, since time.Time) {
	tab.section("Exposure", "regime\tworkload\ttrials\thours\tfailures")
	type exposure struct{ trials, seconds, failures int }
	exposures := map[string]*exposure{}
	for _, t := range p.trials {
		if !selected(t.time, since) {
			continue
		}
		k := fmt.Sprintf("%s\t%s", t.intent.Regime, t.intent.Workload)
		x := exposures[k]
		if x == nil {
			x = &exposure{}
			exposures[k] = x
		}
		if t.started {
			x.trials++
		}
		x.seconds += seconds(t)
		if t.end != nil && t.end.Outcome == journal.OutcomeFailure {
			x.failures++
		}
	}
	for _, k := range keys(exposures) {
		x := exposures[k]
		tab.row("%s\t%d\t%.3f\t%d", k, x.trials, float64(x.seconds)/3600, x.failures)
	}
}

func loadedCCD(t *trial, cores []machine.CoreInfo, ccds map[int]int) string {
	if t == nil {
		return "idle/unknown"
	}
	set := map[string]int{}
	for _, c := range loaded(t.intent, cores) {
		if ccd, ok := ccds[c]; ok {
			set[strconv.Itoa(ccd)]++
		}
	}
	if len(set) == 0 {
		return "idle/unknown"
	}
	return strings.Join(keys(set), ",")
}
