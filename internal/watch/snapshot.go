// Package watch is the read-only dashboard: a projection of the journal, the frame rendered from it and the redraw loop.
package watch

import (
	"errors"
	"io/fs"
	"slices"
	"strings"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

const recentLimit = 100

// Snapshot is everything one frame shows, projected from the journal.
type Snapshot struct {
	problem     error
	session     bool
	guard       bool
	huntID      int
	maskID      int
	round       int
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
	lastFailure *line
	deadEnd     string
	recent      []line
}

func (s Snapshot) Err() error {
	return s.problem
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
	joint        []int
	hunt, masked bool
	anchor       *int
	loaded       bool
}

type trial struct {
	cores      []int
	all        bool
	recordOnly bool
	condition  machine.Condition
	regime     machine.Regime
	workload   string
	started    time.Time
	hasStarted bool
	duration   time.Duration
}

var logged = []journal.Kind{
	journal.KindSessionStart, journal.KindSessionCarried, journal.KindHostRanking,
	journal.KindTrialIntent, journal.KindTrialEnd, journal.KindFailure, journal.KindCrashDetected, journal.KindMCE,
	journal.KindTunerDecision, journal.KindCorePhase, journal.KindGuardRotation, journal.KindGuardStep, journal.KindProfileChange,
	journal.KindHuntStart, journal.KindHuntMask, journal.KindHuntEnd, journal.KindHuntSkipped,
	journal.KindMarkJoint, journal.KindRefineRound, journal.KindTunerWarning, journal.KindSessionWarning, journal.KindBackendRetry,
	journal.KindDeadEnd, journal.KindDefectFound, journal.KindCommandReset, journal.KindShutdown,
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
		session:    true,
		guard:      st.Phase == string(journal.PhaseGuard),
		start:      st.Session.Start,
		current:    -1,
		guardState: st.Guard,
	}
	if st.Hunt != nil {
		s.huntID = st.Hunt.Hunt
		if len(st.Hunt.Masks) > 0 {
			s.maskID = st.Hunt.Masks[len(st.Hunt.Masks)-1].Mask
		}
	}
	if st.Refine != nil {
		s.round = st.Refine.Round
	}
	if n := len(events); n > 0 {
		s.last = events[n-1].Time
	}
	for _, c := range st.Cores {
		tl := tile{id: c.Core, ccd: c.CCD, phase: c.Phase, fail: c.FailedMark}
		for _, mark := range st.JointMarks {
			for _, member := range mark.Members {
				if member.Core == c.Core {
					tl.joint = append(tl.joint, member.Offset)
				}
			}
		}
		if st.Hunt != nil {
			tl.hunt = slices.Contains(st.Hunt.Candidates, c.Core)
		}
		switch c.Phase {
		case journal.PhaseResident, journal.PhaseDone:
			tl.number, tl.hasNumber = c.Offset, true
		case journal.PhaseSearch:
			if c.Pass != nil {
				tl.number, tl.hasNumber = *c.Pass, true
			}
		case journal.PhaseGuard, journal.PhaseHunt, journal.PhaseRefine:
		}
		s.tiles = append(s.tiles, tl)
	}

	var intent *journal.TrialIntent
	lastFailure := -1
	for i := range events {
		e := &events[i]
		switch p := e.Data.(type) {
		case *journal.SessionStart:
			s.order = machine.Order(p.Cores)
		case *journal.TrialIntent:
			if p.Condition == machine.Isolated && p.Core != nil {
				s.current = *p.Core
			}
			if st.InFlight != nil && st.InFlight.Kind == journal.KindTrialIntent && e.Seq == st.InFlight.Seq {
				intent = p
			}
		case *journal.TrialEnd:
			if p.TctlMaxC != nil {
				s.tctlTrial = p.TctlMaxC
			}
		case *journal.Failure:
			s.failures++
			lastFailure = i
		case *journal.CrashDetected:
			s.crashes++
		}
		if st.DeadEnd != nil && e.Seq == st.DeadEnd.Seq {
			s.deadEnd = vtText(e.Msg)
		}
	}
	if lastFailure >= 0 {
		e := &events[lastFailure]
		s.lastFailure = &line{at: e.Time, msg: vtText(e.Msg)}
	}

	switch {
	case intent != nil:
		s.trial = inFlightTrial(intent, len(st.Cores))
		for i := range s.tiles {
			tl := &s.tiles[i]
			tl.loaded = slices.Contains(s.trial.cores, tl.id)
			if intent.Condition == machine.Masked {
				tl.masked = tl.hunt
				if i < len(intent.Profile) {
					if st.Hunt != nil && i < len(st.Hunt.Anchor) && intent.Profile[i] == st.Hunt.Anchor[i] {
						tl.anchor = &st.Hunt.Anchor[i]
					} else {
						tl.trying = &intent.Profile[i]
					}
				}
			} else if tl.loaded && intent.Condition == machine.Isolated && intent.Offset != nil {
				tl.trying = intent.Offset
			}
		}
	case st.InFlight != nil:
		s.inFlight = vtText(st.InFlight.Msg)
	}

	for i := len(events) - 1; i >= 0; i-- {
		e := &events[i]
		if s.trial != nil && !s.trial.hasStarted {
			if p, ok := e.Data.(*journal.TrialStart); ok && p.Trial == intent.Trial {
				s.trial.started, s.trial.hasStarted = e.Time, true
			}
		}
		if len(s.recent) < recentLimit && slices.Contains(logged, e.Kind) {
			if s.recent == nil {
				s.recent = make([]line, 0, min(recentLimit, len(events)))
			}
			s.recent = append(s.recent, line{at: e.Time, msg: vtText(e.Msg)})
		}
		if len(s.recent) == recentLimit && (s.trial == nil || s.trial.hasStarted) {
			break
		}
	}
	slices.Reverse(s.recent)
	return s
}

func inFlightTrial(p *journal.TrialIntent, sessionCores int) *trial {
	cores := p.Cores
	if p.Core != nil {
		cores = []int{*p.Core}
	}
	workload := p.Workload
	if w, ok := machine.WorkloadByID(p.Workload); ok {
		workload = w.Label
	}
	return &trial{
		cores:      cores,
		all:        len(cores) == sessionCores,
		recordOnly: p.RecordOnly,
		condition:  p.Condition,
		regime:     p.Regime,
		workload:   workload,
		duration:   time.Duration(p.DurationS) * time.Second,
	}
}

// vtText drops the degree sign, which the Linux console's default font may lack.
func vtText(msg string) string {
	return strings.ReplaceAll(msg, "°C", " C")
}
