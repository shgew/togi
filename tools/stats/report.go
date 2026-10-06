package main

import (
	"cmp"
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

type entry[K, V any] struct {
	key   K
	value V
}

func sorted[K comparable, V any](m map[K]V, compare func(a, b K) int) []entry[K, V] {
	out := make([]entry[K, V], 0, len(m))
	for k, v := range m {
		out = append(out, entry[K, V]{k, v})
	}
	slices.SortFunc(out, func(a, b entry[K, V]) int { return compare(a.key, b.key) })
	return out
}
func stamp(t time.Time) string                   { return t.UTC().Format(time.RFC3339) }
func selected(t time.Time, since time.Time) bool { return !t.Before(since) }
func seconds(t *trial) int {
	if t.End == nil {
		return 0
	}
	return t.End.DurationS
}
func outcome(t *trial) string {
	if t.End == nil {
		return "open"
	}
	return string(t.End.Outcome)
}
func crash(t *trial) bool {
	return t.Crashed || t.End != nil && t.End.Outcome == journal.OutcomeFailure && t.End.Signal == machine.Crash
}

// metrics holds every figure the report shows, computed before any is rendered.
type metrics struct {
	since        time.Time
	session      sessionMetrics
	runs         []selectedRun
	trialTime    timeMetrics
	failures     failureMetrics
	exposure     []entry[exposureKey, exposure]
	recovery     recoveryGap
	hunts        []huntMetrics
	checking     checkingMetrics
	evidence     evidenceMetrics
	r7           r7Decisions
	depth        []depthRate
	requests     requestMetrics
	inconclusive []*trial
	tctl         []entry[tctlKey, int]
}

func report(out io.Writer, session facts.Session, since time.Time) error {
	return render(out, compute(session, since))
}

func compute(session facts.Session, since time.Time) metrics {
	events := session.Events
	p := project(session)
	downtime, gaps := crashRecovery(p, since)
	return metrics{
		since:        since,
		session:      computeSession(events),
		runs:         selectRuns(p, since),
		trialTime:    timeMetrics{phases: phaseTime(p, since), downtimeS: downtime},
		failures:     computeFailures(p, events, since),
		exposure:     computeExposure(p, since),
		recovery:     summarizeGaps(gaps),
		hunts:        computeHunts(p, since),
		checking:     computeChecking(p, events, since),
		evidence:     computeEvidence(p, events, since),
		r7:           computeR7Decisions(events, since),
		depth:        computeDepth(p, since),
		requests:     computeRequests(r7Measurements(session, p, since)),
		inconclusive: inconclusiveTrials(p, since),
		tctl:         passingTctl(p, since),
	}
}

func render(out io.Writer, m metrics) error {
	tab := &table{out: out}
	renderSession(tab, m.session, m.runs, m.since)
	renderTime(tab, m.trialTime)
	renderFailures(tab, m.failures, m.exposure, m.recovery)
	renderHunts(tab, m.hunts)
	renderChecking(tab, m.checking)
	renderEvidence(tab, m.evidence)
	renderR7Decisions(tab, m.r7)
	renderDepth(tab, m.depth)
	renderRequests(tab, m.requests)
	return renderOutcomes(tab, m.inconclusive, m.tctl)
}

type sessionMetrics struct {
	ids              []string
	rulesets, builds []string
	events           int
	first, last      time.Time
}

func computeSession(events []journal.Event) sessionMetrics {
	var s sessionMetrics
	rules := map[string]int{}
	builds := map[string]int{}
	for _, e := range events {
		switch v := e.Data.(type) {
		case *journal.SessionStart:
			s.ids = append(s.ids, v.Session)
			rules[strconv.Itoa(max(v.Ruleset, 1))]++
		case *journal.ConfigLoaded:
			builds[buildName(v.Build)]++
			if v.Ruleset != 0 {
				rules[strconv.Itoa(v.Ruleset)]++
			}
		}
	}
	s.rulesets, s.builds = keys(rules), keys(builds)
	s.events = len(events)
	if len(events) > 0 {
		s.first, s.last = events[0].Time, events[len(events)-1].Time
	}
	return s
}

type selectedRun struct {
	number int
	*runInfo
}

func selectRuns(p *projection, since time.Time) []selectedRun {
	var runs []selectedRun
	for i, r := range p.runs {
		if selected(r.end, since) {
			runs = append(runs, selectedRun{i + 1, r})
		}
	}
	return runs
}

func renderSession(tab *table, s sessionMetrics, runs []selectedRun, since time.Time) {
	tab.section("Session", "field\tvalue")
	for _, id := range s.ids {
		tab.row("id\t%s", id)
	}
	tab.row("rulesets\t%s", strings.Join(s.rulesets, ", "))
	tab.row("builds\t%s", strings.Join(s.builds, ", "))
	if s.events > 0 {
		tab.row("first\t%s", stamp(s.first))
		tab.row("last\t%s", stamp(s.last))
	}
	if !since.IsZero() {
		tab.row("since\t%s", stamp(since))
	}
	tab.section("Runs", "run\tstart\tend\tbuild\ttrials\tcrashes\tended")
	for _, r := range runs {
		tab.row("%d\t%s\t%s\t%s\t%d\t%d\t%s", r.number, stamp(r.start), stamp(r.end), strings.Join(keys(r.builds), ","), r.trials, r.crashes, r.ending)
	}
}

type phaseKey struct {
	phase     journal.Phase
	condition machine.Condition
	regime    machine.Regime
	outcome   string
}

type timeMetrics struct {
	phases    []entry[phaseKey, int]
	downtimeS float64
}

func phaseTime(p *projection, since time.Time) []entry[phaseKey, int] {
	times := map[phaseKey]int{}
	for _, t := range p.trials {
		if selected(t.Time, since) {
			times[phaseKey{t.Intent.Phase, t.Intent.Condition, t.Intent.Regime, outcome(t)}] += seconds(t)
		}
	}
	return sorted(times, func(a, b phaseKey) int {
		return cmp.Or(cmp.Compare(a.phase, b.phase), cmp.Compare(a.condition, b.condition), cmp.Compare(a.regime, b.regime), cmp.Compare(a.outcome, b.outcome))
	})
}

// crashRecovery returns the seconds from each selected crash's last evidence
// to the start of the next boot, and the gaps to that boot's first event.
func crashRecovery(p *projection, since time.Time) (downtime float64, gaps []float64) {
	for _, t := range p.trials {
		if !selected(t.Time, since) || !crash(t) {
			continue
		}
		if next, ok := p.nextBoot[t.Boot]; ok {
			boot := p.boots[next]
			gaps = append(gaps, boot.firstEvent.Sub(t.LastEvidence).Seconds())
			downtime += boot.start.Sub(t.LastEvidence).Seconds()
		}
	}
	return downtime, gaps
}

func renderTime(tab *table, m timeMetrics) {
	tab.section("Time", "phase / condition / regime / outcome\ttrial seconds\thours")
	for _, x := range m.phases {
		k := x.key
		tab.row("%s / %s / %s / %s\t%d\t%.3f", k.phase, k.condition, k.regime, k.outcome, x.value, float64(x.value)/3600)
	}
	tab.row("crash downtime (last evidence to next boot)\t%.3f\t%.3f", m.downtimeS, m.downtimeS/3600)
}

type failureKey struct {
	regime      machine.Regime
	workload    string
	signal      machine.Signal
	attribution journal.Attribution
}

// crashTiming counts crashes by trial seconds before the last evidence:
// 0, 1-29, 30-59, 60-119 and 120 or more.
type crashTiming [5]int

type failureMetrics struct {
	counts     []entry[failureKey, int]
	loadedCCDs []entry[string, int]
	timing     crashTiming
	resets     []entry[string, int]
}

func computeFailures(p *projection, events []journal.Event, since time.Time) failureMetrics {
	failures := map[failureKey]int{}
	ccds := map[int]int{}
	for _, c := range p.cores {
		ccds[c.Core] = c.CCD
	}
	loadedCCDs := map[string]int{}
	resets := map[string]int{}
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
				workload = tr.Intent.Workload
				if regime == "" {
					regime = tr.Intent.Regime
				}
			}
			failures[failureKey{regime, workload, v.Signal, v.Attribution}]++
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
	var timing crashTiming
	for _, t := range p.trials {
		if selected(t.Time, since) && crash(t) {
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
			timing[b]++
		}
	}
	return failureMetrics{
		counts: sorted(failures, func(a, b failureKey) int {
			return cmp.Or(cmp.Compare(a.regime, b.regime), cmp.Compare(a.workload, b.workload), cmp.Compare(a.signal, b.signal), cmp.Compare(a.attribution, b.attribution))
		}),
		loadedCCDs: sorted(loadedCCDs, strings.Compare),
		timing:     timing,
		resets:     sorted(resets, strings.Compare),
	}
}

type recoveryGap struct {
	count                     int
	shortest, median, longest float64
}

func summarizeGaps(gaps []float64) recoveryGap {
	if len(gaps) == 0 {
		return recoveryGap{}
	}
	slices.Sort(gaps)
	median := gaps[len(gaps)/2]
	if len(gaps)%2 == 0 {
		median = (gaps[len(gaps)/2-1] + median) / 2
	}
	return recoveryGap{len(gaps), gaps[0], median, gaps[len(gaps)-1]}
}

func renderFailures(tab *table, m failureMetrics, exposures []entry[exposureKey, exposure], gap recoveryGap) {
	tab.section("Failures", "regime\tworkload\tsignal\tattribution\tcount")
	for _, x := range m.counts {
		k := x.key
		tab.row("%s\t%s\t%s\t%s\t%d", k.regime, k.workload, k.signal, k.attribution, x.value)
	}
	renderExposure(tab, exposures)
	tab.section("Unattributed crashes by loaded CCD", "CCD(s)\tcount")
	for _, x := range m.loadedCCDs {
		tab.row("%s\t%d", x.key, x.value)
	}
	tab.section("Crash timing (last evidence)", "seconds\tcount")
	for i, k := range []string{"0", "1-29", "30-59", "60-119", "120+"} {
		tab.row("%s\t%d", k, m.timing[i])
	}
	tab.section("Reset reasons", "reason\tcount")
	for _, x := range m.resets {
		tab.row("%s\t%d", x.key, x.value)
	}
	tab.section("Recovery gap", "count\tmin seconds\tmedian seconds\tmax seconds")
	if gap.count > 0 {
		tab.row("%d\t%.3f\t%.3f\t%.3f", gap.count, gap.shortest, gap.median, gap.longest)
	} else {
		tab.row("0\t-\t-\t-")
	}
}

func inconclusiveTrials(p *projection, since time.Time) []*trial {
	var trials []*trial
	for _, t := range p.trials {
		if selected(t.Time, since) && t.End != nil && t.End.Outcome == journal.OutcomeInconclusive {
			trials = append(trials, t)
		}
	}
	return trials
}

type tctlKey struct {
	regime machine.Regime
	maxC   int
}

// passingTctl counts passing trials by regime and peak Tctl.
func passingTctl(p *projection, since time.Time) []entry[tctlKey, int] {
	temps := map[tctlKey]int{}
	for _, t := range p.trials {
		if selected(t.Time, since) && t.End != nil && t.End.Outcome == journal.OutcomePass && t.End.TctlMaxC != nil {
			temps[tctlKey{t.Intent.Regime, *t.End.TctlMaxC}]++
		}
	}
	return sorted(temps, func(a, b tctlKey) int {
		return cmp.Or(cmp.Compare(a.regime, b.regime), cmp.Compare(a.maxC, b.maxC))
	})
}

func renderOutcomes(tab *table, inconclusive []*trial, tctl []entry[tctlKey, int]) error {
	tab.section("Inconclusive trials", "trial\tcondition\treason")
	for _, t := range inconclusive {
		tab.row("%s\t%s\t%s", t.Intent.Trial, t.Intent.Condition, t.End.Reason)
	}
	tab.section("Tctl", "regime\tmax C\tpassing trials")
	for _, x := range tctl {
		tab.row("%s\t%d\t%d", x.key.regime, x.key.maxC, x.value)
	}
	return tab.writer.Flush()
}

type exposureKey struct {
	regime   machine.Regime
	workload string
}

type exposure struct{ trials, seconds, failures int }

func computeExposure(p *projection, since time.Time) []entry[exposureKey, exposure] {
	exposures := map[exposureKey]exposure{}
	for _, t := range p.trials {
		if !selected(t.Time, since) {
			continue
		}
		k := exposureKey{t.Intent.Regime, t.Intent.Workload}
		x := exposures[k]
		if t.Started {
			x.trials++
		}
		x.seconds += seconds(t)
		if t.End != nil && t.End.Outcome == journal.OutcomeFailure {
			x.failures++
		}
		exposures[k] = x
	}
	return sorted(exposures, func(a, b exposureKey) int {
		return cmp.Or(cmp.Compare(a.regime, b.regime), cmp.Compare(a.workload, b.workload))
	})
}

func renderExposure(tab *table, exposures []entry[exposureKey, exposure]) {
	tab.section("Exposure", "regime\tworkload\ttrials\thours\tfailures")
	for _, e := range exposures {
		x := e.value
		tab.row("%s\t%s\t%d\t%.3f\t%d", e.key.regime, e.key.workload, x.trials, float64(x.seconds)/3600, x.failures)
	}
}

func loadedCCD(t *trial, cores []machine.CoreInfo, ccds map[int]int) string {
	if t == nil {
		return "idle/unknown"
	}
	set := map[string]int{}
	for _, c := range loaded(t.Intent, cores) {
		if ccd, ok := ccds[c]; ok {
			set[strconv.Itoa(ccd)]++
		}
	}
	if len(set) == 0 {
		return "idle/unknown"
	}
	return strings.Join(keys(set), ",")
}

// voltageTargetedRuleset is the first ruleset whose multi-core R7 failures move
// by voltage-targeted backoff.
const voltageTargetedRuleset = 9

// r7Backoff is a voltage-targeted backoff; steps is how many offset counts it moved.
type r7Backoff struct {
	seq      int
	decision *journal.TunerDecision
	steps    int
	causes   []int
}

type r7Chain struct {
	seq   int
	chain *journal.CheckingChain
}

type r7Decisions struct {
	backoffs []r7Backoff
	chains   []r7Chain
}

func computeR7Decisions(events []journal.Event, since time.Time) r7Decisions {
	var r r7Decisions
	bySeq := make(map[int]journal.Event, len(events))
	intents := make(map[string]*journal.TrialIntent)
	for _, e := range events {
		bySeq[e.Seq] = e
		if in, ok := e.Data.(*journal.TrialIntent); ok {
			intents[in.Trial] = in
		}
	}
	multiR7 := func(trial string) bool {
		in := intents[trial]
		return in != nil && in.Regime == machine.R7 && len(in.Cores) > 1
	}
	ruleset := 0
	for _, e := range events {
		if p, ok := e.Data.(*journal.SessionStart); ok {
			ruleset = p.Ruleset
		}
		d, ok := e.Data.(*journal.TunerDecision)
		if !ok || !selected(e.Time, since) || d.Decision != journal.Backoff || ruleset < voltageTargetedRuleset {
			continue
		}
		r7 := false
		for _, seq := range e.Cause {
			switch cause := bySeq[seq].Data.(type) {
			case *journal.Failure:
				r7 = r7 || multiR7(cause.Trial)
			case *journal.TrialCarried:
				r7 = r7 || cause.Class.Regime == machine.R7 && len(cause.Class.Cores) > 1
			case *journal.TrialEnd:
				r7 = r7 || multiR7(cause.Trial)
			}
		}
		if r7 {
			r.backoffs = append(r.backoffs, r7Backoff{e.Seq, d, d.ToOffset - d.FromOffset, e.Cause})
		}
	}
	for _, e := range events {
		if d, ok := e.Data.(*journal.CheckingChain); ok && selected(e.Time, since) {
			r.chains = append(r.chains, r7Chain{e.Seq, d})
		}
	}
	return r
}

func renderR7Decisions(tab *table, r r7Decisions) {
	tab.section("R7 voltage-targeted backoffs", "seq\tdecision\tcore\tfrom\tto\tcounts\tcauses\treason")
	for _, b := range r.backoffs {
		d := b.decision
		tab.row("%d\t%s\t%02d\t%d\t%d\t%d\t%v\t%s", b.seq, d.Decision, d.Core, d.FromOffset, d.ToOffset, b.steps, b.causes, journal.EscapeText(d.Reason))
	}
	tab.section("R7 chain derivations", "seq\tcycle\tstep\tCCD\tworkload\tpart\trequest groups\tloaded cores\tsources")
	for _, c := range r.chains {
		d := c.chain
		source := fmt.Sprint(d.SourceSeqs)
		if len(d.SourceSeqs) == 0 {
			source = "offset fallback"
		}
		tab.row("%d\t%d\t%d\t%d\t%s\t%s\t%v\t%s\t%s", c.seq, d.Cycle, d.Step, d.CCD, journal.EscapeText(d.Workload), journal.EscapeText(d.Part), d.Groups, coreList(d.Cores), source)
	}
}
