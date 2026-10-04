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
	problem         error
	session         bool
	start           time.Time
	phase           journal.Phase
	cores           []coreView
	trial           *trial
	inFlight        string
	hunt            *huntView
	deepening       *journal.DeepeningState
	checking        *journal.CheckingState
	order           []int
	trials          int
	shortTrialDuration   time.Duration
	rerunDuration   time.Duration
	canDeepen       bool
	checkingFull    bool
	checkingMissing []string
	failures        int
	crashes         int
	hunts           int
	lastFailure     *failureView
	lastCrash       *time.Time
	deadEnd         *deadEndView
	stopped         *time.Time
	stoppedReason   journal.ShutdownReason
	history         []entry
	log             []entry
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
	queued     bool
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
	recordOnly bool
}

type failureView struct {
	at     time.Time
	signal machine.Signal
}

type huntView struct {
	id         int
	regime     machine.Regime
	loaded     []int
	parked     []int
	candidates []int
	cause      *failureView
	group      *groupView
}

type groupView struct {
	id     int
	stage  string
	cores  []int
	passes int
	needed int
	probe  *journal.CombinationMember
	held   []journal.CombinationMember
}

type deadEndView struct {
	at        time.Time
	condition journal.DeadEndCondition
	detail    string
}

// entry is one line of what happened, in plain words or as the journal recorded it. A pass line counts the runs of
// one test it folds together, each lasting each, and the hottest Tctl any of them reached.
type entry struct {
	at     time.Time
	tag    string
	text   string
	tone   tone
	runs   int
	each   time.Duration
	peak   *int
	reboot int // CrashDetected source sequence, or zero for other entries.
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
	defaults := config.Default()
	s := Snapshot{
		session:       true,
		phase:         journal.Phase(st.Phase),
		start:         st.Session.Start,
		checking:      st.Checking,
		deepening:     st.Deepening,
		trials:        defaults.Evidence.Trials(),
		shortTrialDuration: time.Duration(defaults.Durations.ShortTrialS) * time.Second,
		rerunDuration: time.Duration(t.RerunDuration()) * time.Second,
		canDeepen:     t.CanDeepen(),
	}
	if s.checking != nil {
		s.checkingFull, s.checkingMissing = s.checking.Full, s.checking.Missing
	} else {
		s.checkingFull, s.checkingMissing = t.CheckingCoverage()
	}
	p := projector{s: &s, st: &st, intents: map[string]*journal.TrialIntent{}, applied: map[int]int{}, tuned: map[int]int{}, checking: map[int]bool{}, failures: map[int]*failureView{}}
	for _, e := range events {
		p.fold(e)
	}
	p.finish(events)
	return s
}

type projector struct {
	s           *Snapshot
	st          *journal.State
	intents     map[string]*journal.TrialIntent
	applied     map[int]int
	tuned       map[int]int
	checking    map[int]bool
	current     *journal.TrialIntent
	currentBoot string
	huntStart   *journal.HuntStart
	huntFail    *failureView
	group       *journal.HuntGroup
	failures    map[int]*failureView
	stopped     bool
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
			s.trials = config.Evidence{Miss: ev.Miss, Rate: ev.Rate}.Trials()
		}
		if d.Config.Durations.ShortTrialS > 0 {
			s.shortTrialDuration = time.Duration(d.Config.Durations.ShortTrialS) * time.Second
		}
	case *journal.SMUReadback:
		p.applied[d.Core] = d.Offset
	case *journal.ProfileRestored:
		for i, o := range d.Offsets {
			if i < len(p.st.Cores) {
				p.applied[p.st.Cores[i].Core] = o
			}
		}
	case *journal.TrialIntent:
		p.intents[d.Trial] = d
		p.current = d
		p.currentBoot = e.Boot
	case *journal.TrialEnd:
		if p.current != nil && p.current.Trial == d.Trial {
			p.current = nil
		}
	case *journal.CorePhase:
		p.checking[d.Core] = d.To == journal.PhaseSearch && d.CheckSoloLimit
		p.tuned[d.Core] = d.Offset
	case *journal.TunerDecision:
		p.tuned[d.Core] = d.ToOffset
		if d.Phase == journal.PhaseSearch {
			p.checking[d.Core] = d.Decision == journal.CheckSoloLimit
		}
	case *journal.ProfileChange:
		for i, offset := range d.To {
			if i < len(p.st.Cores) {
				p.tuned[p.st.Cores[i].Core] = offset
			}
		}
	case *journal.Failure:
		s.failures++
		f := &failureView{at: e.Time, signal: d.Signal}
		p.failures[e.Seq] = f
		s.lastFailure = f
	case *journal.CrashDetected:
		if p.current != nil && p.currentBoot == d.PreviousBoot {
			p.current = nil
		}
		if !d.Stray && !d.Inconclusive {
			s.crashes++
			at := e.Time
			s.lastCrash = &at
		}
	case *journal.HuntStart:
		s.hunts++
		p.huntStart, p.group = d, nil
		p.huntFail = p.failures[d.Failure]
	case *journal.HuntGroup:
		p.group = d
	case *journal.DeadEnd:
		s.deadEnd = &deadEndView{at: e.Time, condition: d.Condition, detail: vtText(e.Msg)}
	case *journal.Shutdown:
		p.stopped = true
		at := e.Time
		s.stopped = &at
		s.stoppedReason = d.Reason
	}
	if line, ok := p.describe(e); ok {
		s.history = foldEntry(s.history, line)
		if len(s.history) > 2*historyLimit {
			s.history = slices.Clone(s.history[len(s.history)-historyLimit:])
		}
	}
}

func (p *projector) finish(events []journal.Event) {
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
	for i := len(events) - 1; i >= 0 && len(s.log) < logLimit; i-- {
		e := events[i]
		if e.Kind == journal.KindSMUIntent || e.Kind == journal.KindSMUWrite || e.Kind == journal.KindSMUReadback || e.Kind == journal.KindPreflightCheck {
			continue
		}
		s.log = append(s.log, entry{at: e.Time, text: vtText(e.Msg)})
	}
	slices.Reverse(s.log)
	switch {
	case p.current != nil:
		s.trial = newTrial(p.current, events)
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
		parked: st.Hunt.Parked, candidates: st.Hunt.Candidates, cause: p.huntFail,
	}
	if n := len(st.Hunt.Groups); n > 0 {
		m := st.Hunt.Groups[n-1]
		if m.Outcome == "running" {
			h.group = &groupView{id: m.Group, cores: m.Cores, passes: m.Passes, needed: m.Needed, probe: m.Probe, held: m.Held}
			if p.group != nil && p.group.Group == m.Group {
				h.group.stage = p.group.Stage
			}
		}
	}
	s.hunt = h
}

func (p *projector) coreViews() {
	s, st := p.s, p.st
	showReadback := s.stopped == nil && s.deadEnd == nil || s.deadEnd != nil && s.deadEnd.condition == journal.DeadEndSMU
	for i, c := range st.Cores {
		v := coreView{id: c.Core, ccd: c.CCD, phase: c.Phase, tuned: c.Offset, applied: c.Offset, pass: c.Pass, fail: c.FailurePoint, checking: p.checking[c.Core], queued: c.Queued != ""}
		if a, ok := p.applied[c.Core]; ok && showReadback {
			v.applied = a
		}
		if t := s.trial; t != nil {
			v.loaded = t.hasStarted && slices.Contains(t.cores, c.Core)
			parked := t.condition == machine.Parked && s.hunt != nil && s.hunt.group != nil
			if parked && slices.Contains(s.hunt.candidates, c.Core) {
				g := s.hunt.group
				v.suspect = slices.Contains(g.cores, c.Core) || g.probe != nil && g.probe.Core == c.Core || slices.ContainsFunc(g.held, func(m journal.CombinationMember) bool {
					return m.Core == c.Core
				})
				v.parked = !v.suspect && i < len(s.hunt.parked)
			}
			v.tested = t.hasStarted && (v.suspect || v.loaded && !parked)
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

// newTrial projects the open trial; its latest matching start, wherever it falls, says when it began.
func newTrial(p *journal.TrialIntent, events []journal.Event) *trial {
	cores := p.Cores
	if p.Core != nil {
		cores = []int{*p.Core}
	}
	var started time.Time
	ok := false
	for i := len(events) - 1; i >= 0 && !ok; i-- {
		if d, is := events[i].Data.(*journal.TrialStart); is && d.Trial == p.Trial {
			started, ok = events[i].Time, true
		}
	}
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
		recordOnly: p.RecordOnly,
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
