// Package watch is the read-only dashboard: a projection of the journal, the frame rendered from it and the redraw loop.
package watch

import (
	"errors"
	"io/fs"
	"slices"
	"strings"
	"time"

	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

const (
	historyLimit = 200
	logLimit     = 400
)

// Snapshot is everything one frame shows, projected from the journal.
type Snapshot struct {
	problem     error
	session     bool
	start       time.Time
	phase       journal.Phase
	cores       []coreView
	trial       *trial
	inFlight    string
	hunt        *huntView
	refine      *journal.RefineState
	guard       *journal.GuardState
	order       []int
	starts      int
	failures    int
	crashes     int
	hunts       int
	lastFailure *failureView
	lastCrash   *time.Time
	deadEnd     *deadEndView
	stopped     *time.Time
	history     []entry
	log         []entry
}

func (s Snapshot) Err() error {
	return s.problem
}

type coreView struct {
	id, ccd    int
	phase      journal.Phase
	tuned      int
	applied    int
	pass, fail *int
	checking   bool
	loaded     bool
	tested     bool
	suspect    bool
	parked     bool
}

type trial struct {
	cores      []int
	condition  machine.Condition
	regime     machine.Regime
	workload   string
	offset     *int
	started    time.Time
	hasStarted bool
	duration   time.Duration
	round      int
	rerun      bool
}

type failureView struct {
	at     time.Time
	signal machine.Signal
}

type huntView struct {
	id         int
	regime     machine.Regime
	loaded     []int
	anchor     []int
	candidates []int
	cause      *failureView
	mask       *maskView
}

type maskView struct {
	id     int
	stage  string
	cores  []int
	passes int
	needed int
	edge   *journal.JointMember
}

type deadEndView struct {
	at        time.Time
	condition journal.DeadEndCondition
	detail    string
}

// entry is one line of what happened, in plain words or as the journal recorded it. A pass line counts the runs of
// one test it folds together, each lasting each, and the hottest Tctl any of them reached.
type entry struct {
	at   time.Time
	tag  string
	text string
	tone tone
	runs int
	each time.Duration
	peak *int
}

type tone int

const (
	plainTone tone = iota
	goodTone
	badTone
	warnTone
)

// Load reads the journal in dir without locking it. A torn tail is ignored: the writer is mid-append and the next read
// gets the line.
func Load(dir string) Snapshot {
	events, _, err := journal.Read(dir)
	var incompatible *journal.IncompatibleError
	switch {
	case errors.Is(err, fs.ErrNotExist):
		return Snapshot{}
	case errors.As(err, &incompatible):
		return Snapshot{problem: incompatible}
	case err != nil:
		return Snapshot{problem: err}
	}
	return Project(events)
}

// Project replays events into the snapshot a frame shows.
func Project(events []journal.Event) Snapshot {
	var st journal.State
	t := tuner.New()
	journal.Replay(events, &st, t)
	t.Project(&st)
	if st.Session == nil {
		return Snapshot{}
	}
	s := Snapshot{
		session: true,
		phase:   journal.Phase(st.Phase),
		start:   st.Session.Start,
		guard:   st.Guard,
		refine:  st.Refine,
		starts:  config.Default().Evidence.Starts(),
	}
	p := projector{s: &s, st: &st, intents: map[string]*journal.TrialIntent{}, starts: map[string]time.Time{}, applied: map[int]int{}, checking: map[int]bool{}, failures: map[int]*failureView{}}
	for _, e := range events {
		p.fold(e)
	}
	p.finish()
	return s
}

type projector struct {
	s         *Snapshot
	st        *journal.State
	intents   map[string]*journal.TrialIntent
	starts    map[string]time.Time
	applied   map[int]int
	checking  map[int]bool
	current   *journal.TrialIntent
	huntStart *journal.HuntStart
	huntFail  *failureView
	mask      *journal.HuntMask
	failures  map[int]*failureView
	stopped   bool
}

func (p *projector) fold(e journal.Event) {
	s := p.s
	if e.Kind != journal.KindShutdown && e.Kind != journal.KindProfileRestored && e.Kind != journal.KindSessionWarning {
		p.stopped = false
	}
	switch d := e.Data.(type) {
	case *journal.SessionStart:
		s.order = machine.Order(d.Cores)
	case *journal.ConfigLoaded:
		if ev := d.Config.Evidence; ev.Miss > 0 && ev.Rate > 0 {
			s.starts = config.Evidence{Miss: ev.Miss, Rate: ev.Rate}.Starts()
		}
	case *journal.SMUReadback:
		p.applied[d.Core] = d.Offset
	case *journal.ProfileRestored:
		for i, o := range d.Offsets {
			p.applied[i] = o
		}
	case *journal.TrialIntent:
		p.intents[d.Trial] = d
		p.current = d
	case *journal.TrialEnd:
		if p.current != nil && p.current.Trial == d.Trial {
			p.current = nil
		}
	case *journal.TrialStart:
		p.starts[d.Trial] = e.Time
	case *journal.CorePhase:
		p.checking[d.Core] = d.To == journal.PhaseSearch && d.CheckEdge
	case *journal.TunerDecision:
		if d.Phase == journal.PhaseSearch {
			p.checking[d.Core] = d.Decision == journal.CheckEdge
		}
	case *journal.Failure:
		s.failures++
		f := &failureView{at: e.Time, signal: d.Signal}
		p.failures[e.Seq] = f
		s.lastFailure = f
	case *journal.CrashDetected:
		if !d.Stray && !d.Inconclusive {
			s.crashes++
		}
		at := e.Time
		s.lastCrash = &at
	case *journal.HuntStart:
		s.hunts++
		p.huntStart, p.mask = d, nil
		p.huntFail = p.failures[d.Failure]
	case *journal.HuntMask:
		p.mask = d
	case *journal.DeadEnd:
		s.deadEnd = &deadEndView{at: e.Time, condition: d.Condition, detail: vtText(e.Msg)}
	case *journal.Shutdown:
		p.stopped = true
		at := e.Time
		s.stopped = &at
	}
	if line, ok := p.describe(e); ok {
		s.history = foldEntry(s.history, line)
		if len(s.history) > 2*historyLimit {
			s.history = slices.Clone(s.history[len(s.history)-historyLimit:])
		}
	}
	if e.Kind != journal.KindSMUIntent && e.Kind != journal.KindSMUWrite && e.Kind != journal.KindSMUReadback && e.Kind != journal.KindPreflightCheck {
		s.log = append(s.log, entry{at: e.Time, text: vtText(e.Msg)})
		if len(s.log) > 2*logLimit {
			s.log = slices.Clone(s.log[len(s.log)-logLimit:])
		}
	}
}

func (p *projector) finish() {
	s, st := p.s, p.st
	if !p.stopped {
		s.stopped = nil
	}
	if st.DeadEnd == nil {
		s.deadEnd = nil
	}
	if len(s.history) > historyLimit {
		s.history = s.history[len(s.history)-historyLimit:]
	}
	if len(s.log) > logLimit {
		s.log = s.log[len(s.log)-logLimit:]
	}
	switch {
	case p.current != nil:
		s.trial = newTrial(p.current, p.starts)
	case st.InFlight != nil && st.InFlight.Kind == journal.KindSMUIntent:
		s.inFlight = "setting offsets on the CPU (" + vtText(st.InFlight.Msg) + ")."
	case st.InFlight != nil:
		s.inFlight = vtText(st.InFlight.Msg) + "."
	}
	p.hunt()
	p.coreViews()
}

func (p *projector) hunt() {
	s, st := p.s, p.st
	if st.Hunt == nil || p.huntStart == nil || p.huntStart.Hunt != st.Hunt.Hunt {
		return
	}
	h := &huntView{
		id: st.Hunt.Hunt, regime: p.huntStart.Regime, loaded: p.huntStart.Cores,
		anchor: st.Hunt.Anchor, candidates: st.Hunt.Candidates, cause: p.huntFail,
	}
	if n := len(st.Hunt.Masks); n > 0 {
		m := st.Hunt.Masks[n-1]
		if m.Outcome == "running" {
			h.mask = &maskView{id: m.Mask, cores: m.Cores, passes: m.Passes, needed: m.Needed, edge: m.Edge}
			if p.mask != nil && p.mask.Mask == m.Mask {
				h.mask.stage = p.mask.Stage
			}
		}
	}
	s.hunt = h
}

func (p *projector) coreViews() {
	s, st := p.s, p.st
	for i, c := range st.Cores {
		v := coreView{id: c.Core, ccd: c.CCD, phase: c.Phase, tuned: c.Offset, applied: c.Offset, pass: c.Pass, fail: c.FailedMark, checking: p.checking[c.Core]}
		if a, ok := p.applied[c.Core]; ok && s.stopped == nil && s.deadEnd == nil {
			v.applied = a
		}
		if t := s.trial; t != nil {
			v.loaded = slices.Contains(t.cores, c.Core)
			masked := t.condition == machine.Masked && s.hunt != nil && s.hunt.mask != nil
			if masked && slices.Contains(s.hunt.candidates, c.Core) {
				v.suspect = slices.Contains(s.hunt.mask.cores, c.Core)
				v.parked = !v.suspect && i < len(s.hunt.anchor)
			}
			v.tested = v.suspect || v.loaded && !masked
		}
		s.cores = append(s.cores, v)
	}
}

// foldEntry appends line to history, folding a pass into the line before when that line passed the same test.
func foldEntry(history []entry, line entry) []entry {
	if n := len(history); n > 0 && line.tag == tagPass {
		last := &history[n-1]
		if last.tag == tagPass && last.text == line.text && last.each == line.each {
			last.at, last.runs = line.at, last.runs+line.runs
			if line.peak != nil && (last.peak == nil || *line.peak > *last.peak) {
				last.peak = line.peak
			}
			return history
		}
	}
	return append(history, line)
}

func newTrial(p *journal.TrialIntent, starts map[string]time.Time) *trial {
	cores := p.Cores
	if p.Core != nil {
		cores = []int{*p.Core}
	}
	started, ok := starts[p.Trial]
	return &trial{
		cores:      cores,
		condition:  p.Condition,
		regime:     p.Regime,
		workload:   trialLabel(p),
		offset:     p.Offset,
		started:    started,
		hasStarted: ok,
		duration:   time.Duration(p.DurationS) * time.Second,
		round:      p.Round,
		rerun:      p.Rerun,
	}
}

func workloadLabel(id string) string {
	if w, ok := machine.WorkloadByID(id); ok {
		return w.Label
	}
	return vtText(id)
}

// vtText drops the degree sign, which the Linux console's default font may lack.
func vtText(msg string) string {
	return strings.ReplaceAll(journal.EscapeText(msg), "°C", " C")
}
