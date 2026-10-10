// Package watch projects the journal into a read-only dashboard.
package watch

import (
	"errors"
	"fmt"
	"io/fs"
	"slices"
	"strings"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/render"
	"github.com/shgew/togi/internal/tuner"
)

const (
	historyLimit = 200
	logLimit     = 400
)

func (s Snapshot) Err() error { return s.problem }

func Load(dir string) Snapshot {
	events, _, err := journal.Read(dir)
	return projectRead(events, err)
}

// projectRead is the snapshot of a journal read: empty without a journal, a problem when it cannot be read.
func projectRead(events []journal.Event, err error) Snapshot {
	if errors.Is(err, fs.ErrNotExist) {
		return Snapshot{}
	}
	if err != nil {
		return Snapshot{problem: err}
	}
	return Project(events)
}

func Project(events []journal.Event) Snapshot {
	var st journal.State
	t := tuner.New()
	requirements := requirementRecorder{t: t, intents: map[string]*journal.TrialIntent{}, failed: map[string]tuner.TrialRequirement{}, counts: map[string]trialCount{}, steps: map[string]int{}}
	journal.Replay(events, &st, &requirements, t)
	t.Project(&st)
	if st.Session == nil {
		return Snapshot{}
	}
	s := Snapshot{session: true, start: st.Session.Start, phase: journal.Phase(st.Phase), carried: map[int]bool{}, shapes: map[int]huntShape{}, r7: t.R7Status()}
	for _, c := range st.Cores {
		s.order = append(s.order, c.Core)
	}
	p := projector{s: &s, st: &st, intents: requirements.intents, ends: map[string]*trialEnd{}, groupSignals: map[[2]int]machine.Signal{}, applied: map[int]int{}, tuned: map[int]int{}, solo: map[int]int{}, failures: map[int]*failureView{}, backs: map[int]int{}, sources: map[int]int{}, combinationSeqs: map[int]int{}, probes: map[int]bool{}, requirements: requirements.failed, counts: requirements.counts, steps: requirements.steps, checkHunts: map[[2]int][]int{}, cycleSteps: map[int][]machine.Regime{}, huntStarts: map[int]huntStartView{}, groups: map[[2]int]*journal.HuntGroup{}}
	for _, e := range events {
		p.fold(e)
	}
	p.finish(events, t)
	return s
}

type requirementRecorder struct {
	t       *tuner.State
	intents map[string]*journal.TrialIntent
	failed  map[string]tuner.TrialRequirement
	counts  map[string]trialCount
	steps   map[string]int // the one-based checking step each cycle trial ran in
}

// trialCount is which trial of its checking part, or else of its requirement, a trial was: index of of.
type trialCount struct{ index, of int }

// Fold runs before the tuner folds the same event, so it sees each trial's requirement as the trial ran.
func (r *requirementRecorder) Fold(e journal.Event) {
	switch d := e.Data.(type) {
	case *journal.TrialIntent:
		r.intents[d.Trial] = d
	case *journal.TrialEnd:
		in := r.intents[d.Trial]
		if in == nil {
			return
		}
		req := r.t.Requirement(in)
		if d.Outcome == journal.OutcomeFailure {
			r.failed[d.Trial] = req
		}
		r.counts[d.Trial] = trialCount{req.Trial, req.Needed}
		if in.Cycle > 0 {
			plan := r.t.CyclePlan()
			if part, ok := cyclePartOf(plan, in.Cycle, in.Step, trialCores(in)); ok {
				r.counts[d.Trial] = partCount(part)
			}
			step := in.Step
			if step == 0 && plan.Number == in.Cycle {
				step = plan.Current + 1
			}
			r.steps[d.Trial] = step
		}
	}
}

// cyclePartOf finds the part of a checking step that loads these cores: the part the tuner runs now when one matches,
// else the matching part of the given step, where step 0 means the step the cycle is at.
func cyclePartOf(plan tuner.CyclePlan, cycle, step int, cores []int) (tuner.CyclePart, bool) {
	if plan.Number != cycle {
		return tuner.CyclePart{}, false
	}
	want := slices.Sorted(slices.Values(cores))
	matches := func(part tuner.CyclePart) bool {
		return slices.Equal(slices.Sorted(slices.Values(part.Cores)), want)
	}
	for i, s := range plan.Steps {
		for _, part := range s.Parts {
			if part.Running && matches(part) && (step == 0 || step == i+1) {
				return part, true
			}
		}
	}
	if step == 0 {
		step = plan.Current + 1
	}
	if step < 1 || step > len(plan.Steps) {
		return tuner.CyclePart{}, false
	}
	for _, part := range plan.Steps[step-1].Parts {
		if matches(part) {
			return part, true
		}
	}
	return tuner.CyclePart{}, false
}

// partCount is which trial of its part the next trial is: only passes count toward a part.
func partCount(part tuner.CyclePart) trialCount {
	done := part.Passed
	of := part.Short + part.Long
	return trialCount{min(done+1, max(of, 1)), of}
}

type failureView struct {
	at     time.Time
	signal machine.Signal
}
type projector struct {
	s                    *Snapshot
	st                   *journal.State
	intents              map[string]*journal.TrialIntent
	ends                 map[string]*trialEnd
	groupSignals         map[[2]int]machine.Signal // how each failed hunt group's trial failed
	applied, tuned, solo map[int]int
	current              *journal.TrialIntent
	currentBoot          string
	steps                map[string]int // the one-based checking step each cycle trial ran in
	huntFail             *failureView
	failures             map[int]*failureView
	backs, sources       map[int]int
	combinationSeqs      map[int]int // combination ID by its event's sequence
	probes               map[int]bool
	stopped              bool
	restored             bool
	configs              int
	requirements         map[string]tuner.TrialRequirement
	counts               map[string]trialCount
	checkHunts           map[[2]int][]int
	carrying             bool // solo limits arriving now were carried from an earlier session
	cycleSteps           map[int][]machine.Regime
	huntStarts           map[int]huntStartView
	groups               map[[2]int]*journal.HuntGroup
}

type huntStartView struct {
	at         time.Time
	candidates []int
	named      bool // its first part is on its history line
	live       int  // groups answered by trials run in this session
}

// clearsCombination records which combination a backoff clears. Only a backoff citing the combination itself
// clears it; later decisions merely descend from that one.
func (p *projector) clearsCombination(e journal.Event) {
	if d, ok := e.Data.(*journal.TunerDecision); !ok || d.Decision != journal.Backoff {
		return
	}
	for _, cause := range e.Cause {
		if id := p.combinationSeqs[cause]; id != 0 {
			p.sources[e.Seq] = id
			return
		}
	}
}

func (p *projector) fold(e journal.Event) {
	s := p.s
	p.clearsCombination(e)
	if e.Kind != journal.KindShutdown && e.Kind != journal.KindProfileRestored && e.Kind != journal.KindSessionWarning {
		p.stopped = false
	}
	switch d := e.Data.(type) {
	case *journal.SessionStart:
		p.solo = map[int]int{}
	case *journal.ConfigLoaded:
		p.configs++
	case *journal.SMUIntent:
		// A write after a restoration means the hardware no longer holds what was restored.
		p.restored = false
	case *journal.SMUReadback:
		p.applied[d.Core] = d.Offset
	case *journal.ProfileRestored:
		p.restored = true
		for i, o := range d.Offsets {
			if i < len(p.st.Cores) {
				p.applied[p.st.Cores[i].Core] = o
			}
		}
	case *journal.TrialIntent:
		p.current, p.currentBoot = d, e.Boot
		p.intents[d.Trial] = d
		p.restored = false
	case *journal.TrialStart:
		s.recover = nil
	case *journal.TrialEnd:
		s.last = &trialEnd{id: d.Trial, at: e.Time, outcome: d.Outcome, signal: d.Signal, core: d.Core, duration: time.Duration(d.DurationS) * time.Second, stalled: d.StalledCore, tctlMaxC: d.TctlMaxC, voltageV: d.VoltageRequestMedianV}
		if d.LastSampleS != nil {
			sample := time.Duration(*d.LastSampleS) * time.Second
			s.last.lastSample = &sample
		}
		if in := p.intents[d.Trial]; in != nil {
			s.last.regime = in.Regime
			s.last.cores = trialCores(in)
			s.last.planned = time.Duration(in.DurationS) * time.Second
			if in.Hunt > 0 && d.Outcome == journal.OutcomeFailure {
				p.groupSignals[[2]int{in.Hunt, in.Group}] = d.Signal
			}
		}
		p.ends[d.Trial] = s.last
		if s.recover != nil && s.recover.trial != nil && s.recover.trial.id == d.Trial {
			s.recover.end = s.last
		}
		if p.current != nil && p.current.Trial == d.Trial {
			p.current = nil
		}
	case *journal.CorePhase:
		p.tuned[d.Core] = d.Offset
		switch {
		case d.To == journal.PhaseSearch:
			// A reset core searches again; its old solo limit no longer holds.
			delete(p.solo, d.Core)
		case d.From == journal.PhaseSearch:
			p.solo[d.Core] = d.Offset
		}
	case *journal.TunerDecision:
		p.tuned[d.Core] = d.ToOffset
		if d.Decision == journal.Backoff {
			p.backs[d.Core] = p.sources[e.Seq]
		}
	case *journal.ProfileChange:
		for i, o := range d.To {
			if i < len(p.st.Cores) {
				p.tuned[p.st.Cores[i].Core] = o
			}
		}
	case *journal.Failure:
		s.failures++
		f := &failureView{at: e.Time, signal: d.Signal}
		p.failures[e.Seq] = f
		if d.KnownFailure == 0 {
			at := e.Time
			s.lastFailure = &at
		}
	case *journal.CrashDetected:
		s.crashes++
		r := &recoveryView{crashAt: e.Time, bootAt: e.Time, reset: d.ResetReason}
		if p.current != nil && p.currentBoot == d.PreviousBoot {
			r.trial = newTrial(p.current, nil)
			p.current = nil
		}
		s.recover = r
	case *journal.TrialCarried:
		s.carried[e.Seq] = true
	case *journal.HuntStart:
		s.shapes[d.Hunt] = huntShape{failing: d.Failing, parked: d.Parked}
		p.huntFail = p.failures[d.Failure]
		if in := p.intents[d.Trial]; in != nil && in.Cycle > 0 {
			key := [2]int{in.Cycle, p.steps[d.Trial]}
			p.checkHunts[key] = append(p.checkHunts[key], d.Hunt)
		}
	case *journal.HuntGroup:
		if d.Probe != nil {
			p.probes[d.Hunt] = true
		}
	case *journal.Combination:
		p.combinationSeqs[e.Seq] = d.Combination
	case *journal.DeadEnd:
		s.deadEnd = &deadEndView{at: e.Time, condition: d.Condition, detail: vtText(d.Detail)}
	case *journal.Shutdown:
		p.stopped = true
		s.stopped = &stopView{at: e.Time, reason: d.Reason, saved: p.restored}
	}
	if line, ok := p.describe(e); ok {
		s.history = foldEntry(s.history, line)
		if len(s.history) > 2*historyLimit {
			s.compactHistory()
		}
	}
}

func (s *Snapshot) compactHistory() {
	// The latest group can still gain trials; compact only the prefix, but count a complete tail
	// with the prefix it will join when the final display lines are folded.
	n := len(s.history)
	last := s.history[n-1]
	s.history = append(mergeProbePasses(s.history[:n-1]), last)
	drop := len(s.history) - historyLimit
	if n := len(s.history); n > 1 && mergeableProbePasses(&s.history[n-2], &s.history[n-1]) {
		drop--
	}
	if drop > 0 {
		s.historyDropped += drop
		s.history = slices.Clone(s.history[drop:])
	}
}

func (p *projector) finish(events []journal.Event, t *tuner.State) {
	if !p.stopped {
		p.s.stopped = nil
	}
	if p.st.DeadEnd == nil {
		p.s.deadEnd = nil
	}
	p.recent(events)
	if p.current != nil {
		p.s.trial = newTrial(p.current, events)
		r := t.Requirement(p.current)
		p.s.trial.passed, p.s.trial.index, p.s.trial.of = r.Passed, r.Trial, r.Needed
	}
	p.cyclePlan(t.CyclePlan())
	p.huntPlan(t.HuntPlan(), events)
	p.nameHuntStart()
	turns := t.SearchTurns()
	for _, tr := range turns {
		p.s.turns = append(p.s.turns, turnView{core: tr.Core, confirm: tr.Confirm, regimes: tr.Regimes, offset: tr.Offset, workload: tr.Workload, step: tr.Step, running: tr.Running})
	}
	dp := t.DeepeningPlan()
	p.s.deepen = &deepenView{round: dp.Round, room: dp.Room, profile: dp.Profile, checks: dp.Checks, waiting: dp.Waiting}
	p.coreViews(t, turns)
	p.combinations()
	p.forecasts(tuner.Forecast(events))
}

func (p *projector) recent(events []journal.Event) {
	s := p.s
	s.history = mergeProbePasses(s.history)
	if len(s.history) > historyLimit {
		s.historyDropped += len(s.history) - historyLimit
		s.history = s.history[len(s.history)-historyLimit:]
	}
	slices.Reverse(s.history)
	for _, e := range slices.Backward(events) {
		if e.Kind == journal.KindSMUIntent || e.Kind == journal.KindSMUWrite || e.Kind == journal.KindSMUReadback || e.Kind == journal.KindPreflightCheck {
			continue
		}
		s.logTotal++
		if len(s.log) < logLimit {
			s.log = append(s.log, entry{at: e.Time, tag: journalTag(e, p.intents), text: vtText(e.Msg)})
		}
	}
	slices.Reverse(s.log)
}

func (p *projector) cyclePlan(cp tuner.CyclePlan) {
	s := p.s
	if p.st.Checking != nil {
		s.cleanCycles = p.st.Checking.CleanCycles
	}
	if cp.Number == 0 && len(cp.Steps) == 0 {
		return
	}
	s.cycle = &cycleView{number: cp.Number, open: cp.Open, current: cp.Current, paused: cp.Paused}
	for i, step := range cp.Steps {
		v := cycleStep{regime: step.Regime, workload: step.Workload, done: step.Done, more: !step.ChainsComplete, hunts: p.checkHunts[[2]int{cp.Number, i + 1}]}
		for _, part := range step.Parts {
			v.parts = append(v.parts, cyclePart{cores: part.Cores, ccd: part.CCD, full: part.Full, short: part.Short, shortLen: time.Duration(part.ShortS) * time.Second, long: part.Long, longLen: time.Duration(part.LongS) * time.Second, passed: part.Passed, failed: part.Failed, running: part.Running, done: part.Done})
		}
		s.cycle.steps = append(s.cycle.steps, v)
	}
	if s.trial != nil {
		p.placeInCycle(s.trial)
	}
	if s.recover != nil && s.recover.trial != nil {
		tr := s.recover.trial
		p.placeInCycle(tr)
		if count, ok := p.counts[tr.id]; ok {
			tr.index, tr.of = count.index, count.of
		}
	}
}

// placeInCycle finds the step and part of its checking cycle a trial loads, and which trial of that part it is.
func (p *projector) placeInCycle(tr *trialView) {
	g := p.s.cycle
	if g == nil || tr.cycle != g.number {
		return
	}
	want := slices.Sorted(slices.Values(tr.cores))
	matches := func(part cyclePart) bool { return slices.Equal(slices.Sorted(slices.Values(part.cores)), want) }
	place := func(step, index int) {
		parts := g.steps[step].parts
		part := parts[index]
		tr.step, tr.part, tr.parts = step+1, index+1, len(parts)
		count := partCount(tuner.CyclePart{Short: part.short, Long: part.long, Passed: part.passed, Failed: part.failed})
		tr.passed, tr.index, tr.of = part.passed, count.index, count.of
	}
	for i, step := range g.steps {
		for j, part := range step.parts {
			if part.running && matches(part) && (tr.step == 0 || tr.step == i+1) {
				place(i, j)
				return
			}
		}
	}
	step := tr.step
	if step == 0 {
		step = g.current + 1
	}
	if step < 1 || step > len(g.steps) {
		return
	}
	for j, part := range g.steps[step-1].parts {
		if matches(part) {
			place(step-1, j)
			return
		}
	}
}

func (p *projector) huntPlan(hp *tuner.HuntPlan, events []journal.Event) {
	if hp == nil {
		return
	}
	h := &huntView{id: hp.Number, regime: hp.Regime, candidates: hp.Candidates}
	rp := hp.Rerun
	h.rerun = rerunPlan{regime: rp.Regime, cores: rp.Cores, short: rp.Short, shortLen: time.Duration(rp.ShortS) * time.Second, long: rp.Long, longLen: time.Duration(rp.LongS) * time.Second}
	if p.huntFail != nil {
		h.cause.at, h.cause.signal = p.huntFail.at, p.huntFail.signal
	}
	if in := p.intents[hp.Trial]; in != nil {
		h.cause.trial = *newTrial(in, events)
		h.cause.rerunOf = in.Rerun
		r := p.requirements[in.Trial]
		h.cause.trialNum = r.Trial
		h.cause.trial.passed, h.cause.trial.index, h.cause.trial.of = r.Passed, r.Trial, r.Needed
		p.placeInCycle(&h.cause.trial)
		h.cause.end = p.ends[in.Trial]
	}
	if hs := p.st.Hunt; hs != nil && hs.Hunt == hp.Number {
		shape := p.s.shapes[hp.Number]
		shape.parked = hs.Parked
		p.s.shapes[hp.Number] = shape
		h.parkedZero = true
		for i, offset := range hs.Parked {
			if i < len(shape.failing) && shape.failing[i] != offset && offset != 0 {
				h.parkedZero = false
			}
		}
	}
	for _, e := range events {
		switch d := e.Data.(type) {
		case *journal.HuntStart:
			if d.Hunt == hp.Number {
				h.started = e.Time
			}
		case *journal.Failure:
			if e.Seq == hp.FailureSeq {
				h.cause.core, h.cause.known, h.cause.carried, h.cause.regime = d.Core, d.KnownFailure != 0, p.s.carried[d.KnownFailure], d.Regime
			}
		case *journal.TrialCarried:
			// A skip of a known carried failure starts the hunt from that carried fact.
			if e.Seq == hp.FailureSeq {
				h.cause.known, h.cause.carried, h.cause.regime = true, true, d.Class.Regime
			}
		case *journal.FailureCarried:
			if e.Seq == hp.FailureSeq {
				// An idle crash, not a trial: no regime, whatever ledger class it was filed under.
				h.cause.core, h.cause.known, h.cause.carried, h.cause.regime = d.Core, true, true, ""
			}
		}
	}
	h.cut = hp.Cut
	for _, part := range hp.Parts {
		h.plan = append(h.plan, huntPart{failing: part.Failing, parked: part.Parked, trials: part.Trials, length: time.Duration(part.DurationS) * time.Second, group: part.Group, outcome: part.Outcome, running: part.Running})
	}
	for _, g := range hp.Groups {
		h.groups = append(h.groups, groupView{id: g.Number, cores: g.Cores, profile: g.Profile, stage: g.Stage, probe: g.Probe, held: g.Held, outcome: g.Outcome, signal: p.groupSignals[[2]int{hp.Number, g.Number}], passes: g.Passed, needed: g.Needed, inferred: g.Carried})
	}
	for _, pr := range hp.Probes {
		h.probes = append(h.probes, probeView{member: pr.Member, now: pr.Offset, failedAt: pr.FailedAt, passedAt: pr.PassedAt, carried: pr.Carried, running: pr.Running, done: pr.Done})
	}
	if tr := p.s.trial; tr != nil && tr.hunt == hp.Number {
		tr.huntParts, tr.huntCut = len(h.plan), h.cut
		for i, part := range h.plan {
			if part.running {
				tr.huntPart = i + 1
			}
		}
		for _, g := range h.groups {
			if g.id == tr.group {
				tr.probe = g.probe
			}
		}
	}
	p.s.hunt = h
}

// nameHuntStart puts the first part of the running hunt on its start line before the part's group is recorded.
func (p *projector) nameHuntStart() {
	h := p.s.hunt
	if h == nil || len(h.plan) == 0 || p.huntStarts[h.id].named {
		return
	}
	for i := range p.s.history {
		if e := &p.s.history[i]; e.key == "hunt start" && e.hunt == h.id {
			e.text = fmt.Sprintf("#%d started · part 1: %s", h.id, partLayout(h.plan[0].failing, h.candidates))
		}
	}
}

func (p *projector) coreViews(t *tuner.State, turns []tuner.SearchTurn) {
	for i, c := range p.st.Cores {
		v := p.coreView(c, t, turns)
		p.trialRole(&v, i)
		if p.s.hunt != nil {
			for _, g := range p.s.hunt.groups {
				member := slices.Contains(g.cores, c.Core) || g.probe != nil && g.probe.Core == c.Core
				if (g.outcome == "failure" || g.outcome == "fail") && member && i < len(g.profile) && !slices.Contains(v.groupFails, g.profile[i]) {
					v.groupFails = append(v.groupFails, g.profile[i])
				}
			}
		}
		p.s.cores = append(p.s.cores, v)
	}
	slices.SortFunc(p.s.cores, func(a, b coreView) int { return a.id - b.id })
	if tr := p.s.trial; tr != nil {
		for _, c := range p.s.cores {
			if !slices.Contains(tr.cores, c.id) && !slices.Contains(tr.parked, c.id) {
				tr.idle = append(tr.idle, c.id)
			}
		}
	}
}

func (p *projector) coreView(c journal.CoreState, t *tuner.State, turns []tuner.SearchTurn) coreView {
	v := coreView{id: c.Core, ccd: c.CCD, profile: c.Offset, applied: c.Offset, pass: c.Pass, fail: c.FailurePoint, queued: vtText(c.Queued)}
	if a, ok := p.applied[c.Core]; ok {
		v.applied = a
	}
	if solo, ok := p.solo[c.Core]; ok {
		v.solo = &solo
	}
	if v.solo != nil && v.profile > *v.solo {
		v.gaveBack = p.backs[c.Core]
	}
	switch c.Phase {
	case journal.PhaseSearch:
		v.state = coreWaiting
	case journal.PhaseAtLimit:
		v.state = coreAtLimit
	case journal.PhaseHasRoom, journal.PhaseChecking, journal.PhaseHunt, journal.PhaseDeepening:
		v.state = coreHasRoom
	default:
		v.state = coreWaiting
	}
	if len(turns) > 0 && c.Phase != journal.PhaseSearch && v.solo != nil {
		v.state = coreFound
	}
	for _, tr := range turns {
		if tr.Core != c.Core {
			continue
		}
		v.next = &tr.Offset
		v.state = coreSearch
		if tr.Confirm {
			v.state = coreConfirm
			v.confirm = &confirmView{offset: tr.Offset, light: tr.Light, heavy: tr.Heavy, needed: tr.Needed}
		}
		if !tr.Running && c.Offset == 0 && c.Pass == nil {
			v.state = coreWaiting
		}
	}
	if p.s.hunt != nil && slices.Contains(p.s.hunt.candidates, c.Core) {
		v.state = coreSuspect
	}
	l := t.CoreLimit(c.Core)
	if l.Floor || l.Failure || l.Combination != 0 {
		v.holder = &limit{floor: l.Floor, failure: l.Failure, combination: l.Combination}
	}
	return v
}

func (p *projector) trialRole(v *coreView, index int) {
	tr, h := p.s.trial, p.s.hunt
	if tr == nil {
		return
	}
	v.loaded = tr.hasStarted && slices.Contains(tr.cores, v.id)
	v.judged = v.loaded
	if h == nil || tr.condition != machine.Parked {
		return
	}
	member := false
	for _, g := range h.groups {
		if g.outcome != "running" {
			continue
		}
		member = slices.Contains(g.cores, v.id) || g.probe != nil && g.probe.Core == v.id || slices.ContainsFunc(g.held, func(m journal.CombinationMember) bool { return m.Core == v.id })
		if g.probe != nil && g.probe.Core == v.id {
			v.state = coreProbe
		}
	}
	if member {
		if v.state != coreProbe {
			v.state = coreMember
		}
		v.judged = tr.hasStarted
		return
	}
	parked := index < len(p.st.Hunt.Parked) && index < len(tr.profile) && tr.profile[index] == p.st.Hunt.Parked[index]
	v.judged = v.loaded && !parked
	if parked {
		tr.parked = append(tr.parked, v.id)
	}
	if slices.Contains(h.candidates, v.id) {
		v.state = coreParked
		o := v.profile
		v.returnsTo = &o
	}
}

func (p *projector) combinations() {
	for _, c := range p.st.Combinations {
		v := comboView{id: c.Combination, members: c.Members, hunt: c.Hunt, fallback: c.Fallback, probed: p.probes[c.Hunt]}
		for _, m := range c.Members {
			for _, core := range p.s.cores {
				if core.id != m.Core {
					continue
				}
				if core.profile > m.Offset {
					v.clear = append(v.clear, m.Core)
				}
				if core.holder != nil && core.holder.combination == c.Combination {
					held := journal.CombinationMember{Core: core.id, Offset: core.profile}
					v.holds = &held
				}
			}
		}
		p.s.combos = append(p.s.combos, v)
	}
}

func (p *projector) forecasts(forecast tuner.ForecastPlan) {
	p.s.next = atStep(forecast.Next, forecast.NextStep)
	for _, b := range forecast.Branches {
		pr := ifPasses
		switch b.Premise {
		case tuner.IfPass:
		case tuner.IfAllPass:
			pr = ifAllPass
		case tuner.IfNamed:
			pr = ifNamed
		case tuner.IfUnnamed:
			pr = ifUnnamed
		case tuner.IfInconclusive:
			pr = ifInconclusive
		}
		p.s.outcomes = append(p.s.outcomes, outcome{premise: pr, passes: b.Passes, decisions: b.Decisions, next: atStep(b.Next, b.NextStep), core: b.Core, atZero: pr == ifNamed && b.AtZero, top: pr == ifNamed && b.TopRequester, needsRanking: b.NeedsRanking, needsHistory: b.NeedsHistory, withoutTelemetry: b.WithoutTelemetry, offsetOrder: b.OffsetOrder})
	}
}

// atStep places a forecast trial at the checking step the tuner reports for it; intents record only partial steps.
func atStep(t *tuner.Trial, step int) *tuner.Trial {
	if t == nil || t.Step != 0 || step == 0 {
		return t
	}
	placed := *t
	placed.Step = step
	return &placed
}

func journalTag(e journal.Event, intents map[string]*journal.TrialIntent) string {
	switch d := e.Data.(type) {
	case *journal.TrialEnd:
		if in := intents[d.Trial]; in != nil && in.RecordOnly {
			return tagRecord
		}
		if d.Outcome == journal.OutcomePass {
			return tagPass
		}
		if d.Signal == machine.Crash {
			return tagCrash
		}
		if d.Outcome == journal.OutcomeFailure {
			return tagFail
		}
		return tagUnclear
	case *journal.Failure:
		if d.KnownFailure != 0 {
			return tagSkip
		}
		return tagFail
	case *journal.CrashDetected:
		return tagCrash
	case *journal.HuntGroup:
		if d.Skipped || d.Inferred != "" {
			return tagSkip
		}
		if d.Probe != nil {
			return tagProbe
		}
		return tagGroup
	case *journal.HuntStart, *journal.HuntEnd, *journal.HuntSkipped:
		return tagHunt
	case *journal.Combination:
		return tagCombo
	case *journal.TunerDecision:
		tag, _, _ := decisionText(d)
		return tag
	case *journal.CorePhase:
		tag, _, _ := phaseText(d)
		if tag != "" {
			return tag
		}
		return tagLimit
	case *journal.CheckingCycle:
		return tagCycle
	case *journal.CheckingStep:
		return tagSearch
	case *journal.SessionStart:
		return tagStart
	case *journal.Shutdown, *journal.DeadEnd:
		return tagStop
	}
	return vtText(strings.ReplaceAll(string(e.Kind), ".", " "))
}

func trialCores(p *journal.TrialIntent) []int {
	if p.Core != nil {
		return []int{*p.Core}
	}
	return slices.Clone(p.Cores)
}
func newTrial(p *journal.TrialIntent, events []journal.Event) *trialView {
	tr := &trialView{id: p.Trial, cores: trialCores(p), condition: p.Condition, regime: p.Regime, phase: p.Phase, profile: slices.Clone(p.Profile), duration: time.Duration(p.DurationS) * time.Second, round: p.Round, rerun: p.Rerun, retry: p.Retry, recordOnly: p.RecordOnly, partial: partialTrial(p, events), cycle: p.Cycle, step: p.Step, hunt: p.Hunt, group: p.Group}
	tr.workload, _ = machine.WorkloadByID(p.Workload)
	if tr.workload.Label == "" {
		tr.workload.Label = vtText(p.Workload)
		tr.workload.ID = vtText(p.Workload)
	}
	if p.Core != nil {
		tr.core = *p.Core
	}
	if p.Offset != nil {
		tr.offset = *p.Offset
	}
	for _, e := range slices.Backward(events) {
		if d, ok := e.Data.(*journal.TrialStart); ok && d.Trial == p.Trial {
			tr.started, tr.hasStarted = e.Time, true
			break
		}
	}
	return tr
}

func partialTrial(p *journal.TrialIntent, events []journal.Event) bool {
	if p.Regime != machine.R7 {
		return false
	}
	if p.RecordOnly {
		return true
	}
	for _, e := range events {
		if start, ok := e.Data.(*journal.SessionStart); ok {
			for _, core := range start.Cores {
				if !slices.Contains(p.Cores, core.Core) {
					continue
				}
				for _, other := range start.Cores {
					if other.CCD == core.CCD && !slices.Contains(p.Cores, other.Core) {
						return true
					}
				}
			}
			return false
		}
	}
	return false
}

func workloadLabel(id string) string {
	if w, ok := machine.WorkloadByID(id); ok {
		return w.Label
	}
	return vtText(id)
}
func vtText(msg string) string { return render.EscapeText(msg) }
