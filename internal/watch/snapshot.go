// Package watch is the read-only dashboard: a projection of the journal, the frame rendered from it and the redraw loop.
package watch

import (
	"errors"
	"io/fs"
	"slices"
	"strings"
	"time"

	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
	"code.marleb.org/shgew/shycler/internal/tuner"
)

const recentLimit = 100

// Snapshot is everything one frame shows, projected from the journal.
type Snapshot struct {
	problem     string
	session     bool
	guard       bool
	start, last time.Time
	tiles       []tile
	trial       *trial
	inFlight    string
	order       []int
	current     int
	failures    int
	crashes     int
	tctlTrial   *int
	guardState  *journal.GuardState
	tier        journal.Tier
	lastFailure *line
	deadEnd     string
	recent      []line
}

type line struct {
	at  time.Time
	msg string
}

type tile struct {
	id, ccd      int
	phase        journal.Phase
	number       int
	hasNumber    bool
	fail, trying *int
	slots, total int
	regain       int
	settled      int
	loaded       bool
}

type trial struct {
	cores      []int
	all        bool
	condition  machine.Condition
	regime     machine.Regime
	workload   string
	started    time.Time
	hasStarted bool
	duration   time.Duration
}

var logged = []journal.Kind{
	journal.KindSessionStart, journal.KindTrialIntent, journal.KindTrialEnd, journal.KindFailure, journal.KindCrashDetected,
	journal.KindTunerDecision, journal.KindCorePhase, journal.KindGuardRotation, journal.KindProfileChange,
	journal.KindTierChange, journal.KindDeadEnd, journal.KindDefectFound, journal.KindCommandReset, journal.KindShutdown,
}

// Load reads the journal in dir without locking it. A torn tail is ignored: the writer is mid-append and the next read
// gets the line.
func Load(dir string) Snapshot {
	events, _, err := journal.Read(dir)
	var incompatible *journal.IncompatibleError
	switch {
	case errors.Is(err, fs.ErrNotExist):
		return Snapshot{}
	case errors.As(err, &incompatible):
		return Snapshot{problem: incompatible.Error()}
	case err != nil:
		return Snapshot{problem: err.Error()}
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
		session:    true,
		guard:      st.Phase == "guard",
		start:      st.Session.Start,
		current:    -1,
		guardState: st.Guard,
		tier:       st.Tier,
	}
	if n := len(events); n > 0 {
		s.last = events[n-1].Time
	}
	for _, c := range st.Cores {
		tl := tile{id: c.Core, ccd: c.CCD, phase: c.Phase, fail: c.FailedMark}
		tl.slots, tl.total = t.Confirmation(c.Core)
		switch c.Phase {
		case journal.PhaseConfirmed, journal.PhaseGuard:
			tl.number, tl.hasNumber = c.Offset, true
			tl.regain = c.UnprovenDepth - c.SettledDepth
			tl.settled = c.SettledDepth
		case journal.PhaseSearch, journal.PhaseConfirmation:
			if c.Pass != nil {
				tl.number, tl.hasNumber = *c.Pass, true
			}
		}
		s.tiles = append(s.tiles, tl)
	}

	var intent *journal.TrialIntent
	starts := map[string]time.Time{}
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.SessionStart:
			s.order = tuner.Order(p.Cores)
		case *journal.TrialIntent:
			if p.Condition == machine.Isolated && p.Core != nil {
				s.current = *p.Core
			}
			if st.InFlight != nil && st.InFlight.Kind == journal.KindTrialIntent && e.Seq == st.InFlight.Seq {
				intent = p
			}
		case *journal.TrialStart:
			starts[p.Trial] = e.Time
		case *journal.TrialEnd:
			if p.TctlMaxC != nil {
				s.tctlTrial = p.TctlMaxC
			}
		case *journal.Failure:
			s.failures++
			s.lastFailure = &line{at: e.Time, msg: vtText(e.Msg)}
		case *journal.CrashDetected:
			s.crashes++
		}
		if st.DeadEnd != nil && e.Seq == st.DeadEnd.Seq {
			s.deadEnd = vtText(e.Msg)
		}
		if slices.Contains(logged, e.Kind) {
			s.recent = append(s.recent, line{at: e.Time, msg: vtText(e.Msg)})
		}
	}
	if len(s.recent) > recentLimit {
		s.recent = s.recent[len(s.recent)-recentLimit:]
	}

	switch {
	case intent != nil:
		s.trial = inFlightTrial(intent, starts, len(st.Cores))
		for i := range s.tiles {
			tl := &s.tiles[i]
			if !slices.Contains(s.trial.cores, tl.id) {
				continue
			}
			tl.loaded = true
			if intent.Condition == machine.Isolated && intent.Offset != nil {
				tl.trying = intent.Offset
			}
		}
	case st.InFlight != nil:
		s.inFlight = vtText(st.InFlight.Msg)
	}
	return s
}

func inFlightTrial(p *journal.TrialIntent, starts map[string]time.Time, sessionCores int) *trial {
	cores := p.Cores
	if p.Core != nil {
		cores = []int{*p.Core}
	}
	workload := p.Workload
	if w, ok := machine.WorkloadByID(p.Workload); ok {
		workload = w.Label
	}
	started, ok := starts[p.Trial]
	return &trial{
		cores:      cores,
		all:        len(cores) == sessionCores,
		condition:  p.Condition,
		regime:     p.Regime,
		workload:   workload,
		started:    started,
		hasStarted: ok,
		duration:   time.Duration(p.DurationS) * time.Second,
	}
}

// vtText drops the degree sign, which the Linux console's default font may lack.
func vtText(msg string) string {
	return strings.ReplaceAll(msg, "°C", " C")
}
