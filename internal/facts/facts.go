// Package facts extracts decisive trial outcomes and idle failures of applied
// profiles from journals. A fact preserves its source and the full profile;
// inconclusive and unfinished trials are not stability evidence.
package facts

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type Kind string

const (
	TrialFact Kind = "trial"
	IdleFact  Kind = "idle"
)

// Class uses the tuner's ledger key: condition and phase are not part of it.
// Idle facts have R6 and all cores, with wildcard workload and duration (zero).
type Class = journal.TrialClass

// Fact.Seq and Time name the decisive end or idle failure. Build and Boot name
// the trial intent (the idle failure itself for idle facts). DurationS is measured,
// while Class.DurationS is intended. Rerun retains the recorded flag beside Phase.
type Fact struct {
	Kind       Kind
	Session    string
	Seq        int
	Time       time.Time
	Build      journal.Build
	Ruleset    int
	Epoch      int
	Trial      string
	Boot       string
	Class      Class
	Condition  machine.Condition
	Phase      journal.Phase
	Rerun      bool
	RecordOnly bool `json:"record_only,omitempty"`
	Profile    []int
	Outcome    journal.Outcome
	Signal     machine.Signal
	DurationS  int
	Core       *int
	Idle       *journal.Failure
}

// Trial retains nondecisive trials too, for readers reporting interrupted work.
// Intent.Profile is reconstructed for old journals and is in core-ID order.
// Build and Boot belong to the intent, even when a later build closes the trial.
type Trial struct {
	Intent       *journal.TrialIntent
	Seq          int
	Time         time.Time
	Boot         string
	Build        journal.Build
	Cause        []int
	End          *journal.TrialEnd
	EndSeq       int
	Started      bool
	LastEvidence time.Time
	Crashed      bool
}

// Reset retains the request and its recorded effects. AppliedSeq is zero until
// the matching core.phase or session.archived; facts are never removed by a reset.
type Reset struct {
	Seq        int
	Core       *int
	All        bool
	AppliedSeq int
	Effects    []journal.Event
}

// Session.Schema and Ruleset are its opening stamp. Builds lists distinct stamps
// in first-seen order. A nil Context means no BIOS context was recorded.
type Session struct {
	ID       string
	Path     string
	Archived bool
	Context  *machine.BIOSContext
	Schema   int
	Ruleset  int
	Epoch    int
	Builds   []journal.Build
	Resets   []Reset
	Facts    []Fact
	Carried  []Fact
	Cores    []machine.CoreInfo
	Trials   []*Trial
	Events   []journal.Event
}

func ReadDir(stateDir string) ([]Session, error) {
	archive := filepath.Join(stateDir, "archive")
	entries, err := os.ReadDir(archive)
	if err != nil && !errors.Is(err, fs.ErrNotExist) {
		return nil, fmt.Errorf("read fact archives: %w", err)
	}
	var sessions []Session
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".jsonl") {
			continue
		}
		s, err := ReadJournal(filepath.Join(archive, entry.Name()))
		if err != nil {
			return nil, err
		}
		s.Archived = true
		sessions = append(sessions, s)
	}
	live, err := ReadJournal(filepath.Join(stateDir, "events.jsonl"))
	if err == nil {
		sessions = append(sessions, live)
	} else if !errors.Is(err, fs.ErrNotExist) {
		return nil, err
	}
	slices.SortStableFunc(sessions, func(a, b Session) int {
		return journal.CompareSessionIDs(a.ID, b.ID)
	})
	return sessions, nil
}

func ReadJournal(path string) (Session, error) {
	events, err := journal.ReadHistory(path)
	if err != nil {
		return Session{}, err
	}
	s := FromEvents(events)
	s.Path = path
	s.Archived = filepath.Base(filepath.Dir(path)) == "archive"
	return s, nil
}

// FromEvents projects already-decoded events without altering their payloads.
// Events remain available for readers needing observations other than facts.
func FromEvents(events []journal.Event) Session {
	s := Session{Events: events}
	byID := map[string]*Trial{}
	bySeq := map[int]*Trial{}
	resetBySeq := map[int]int{}
	var applied []int
	var ids []int
	var build journal.Build
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.SessionStart:
			s.ID, s.Schema = p.Session, p.Schema
			s.Cores = slices.Clone(p.Cores)
			for _, c := range p.Cores {
				ids = append(ids, c.Core)
			}
			slices.Sort(ids)
			build = normalizedBuild(p.Build)
			s.Ruleset = build.Ruleset
			s.Epoch = p.Epoch()
			s.Builds = append(s.Builds, build)
		case *journal.SessionContext:
			context := p.BIOSContext
			s.Context = &context
		case *journal.ConfigLoaded:
			if p.Version == "" {
				continue
			}
			build = normalizedBuild(p.Build)
			if !slices.Contains(s.Builds, build) {
				s.Builds = append(s.Builds, build)
			}
		case *journal.ProfileApplied:
			applied = slices.Clone(p.Offsets)
		case *journal.ProfileChange:
			applied = slices.Clone(p.To)
		case *journal.SMUReadback:
			if i := slices.Index(ids, p.Core); i >= 0 && i < len(applied) {
				applied[i] = p.Offset
			}
		case *journal.TrialIntent:
			intent := *p
			intent.Profile = trialProfile(p, ids, applied)
			t := &Trial{Intent: &intent, Seq: e.Seq, Time: e.Time, Boot: e.Boot, Build: build, Cause: e.Cause, LastEvidence: e.Time}
			s.Trials = append(s.Trials, t)
			byID[p.Trial], bySeq[e.Seq] = t, t
		case *journal.TrialStart:
			if t := byID[p.Trial]; t != nil {
				t.Started, t.LastEvidence = true, e.Time
			}
		case *journal.TrialProgress:
			trialEvidence(byID[p.Trial], e)
		case *journal.TrialSignal:
			trialEvidence(byID[p.Trial], e)
		case *journal.TrialSample:
			trialEvidence(byID[p.Trial], e)
		case *journal.CrashDetected:
			if p.InFlight != nil {
				if t := bySeq[*p.InFlight]; t != nil && t.Boot == p.PreviousBoot {
					t.Crashed = true
				}
			}
		case *journal.TrialEnd:
			s.recordTrialEnd(e, p, byID[p.Trial])
		case *journal.Failure:
			s.recordIdleFailure(e, p, build, ids)
		case *journal.TrialCarried:
			s.Carried = append(s.Carried, carriedTrial(p))
		case *journal.FailureCarried:
			s.Carried = append(s.Carried, carriedFailure(p))
		case *journal.CommandReset:
			resetBySeq[e.Seq] = len(s.Resets)
			s.Resets = append(s.Resets, Reset{Seq: e.Seq, Core: p.Core, All: p.All})
		}
		s.recordResetEffects(e, resetBySeq)
	}
	return s
}

func (s *Session) recordTrialEnd(e journal.Event, end *journal.TrialEnd, t *Trial) {
	if t == nil {
		return
	}
	t.End, t.EndSeq = end, e.Seq
	if end.Outcome != journal.OutcomePass && end.Outcome != journal.OutcomeFailure {
		return
	}
	in := t.Intent
	s.Facts = append(s.Facts, Fact{Kind: TrialFact, Session: s.ID, Seq: e.Seq, Time: e.Time, Build: t.Build, Ruleset: t.Build.Ruleset, Epoch: s.Epoch, Trial: end.Trial, Boot: t.Boot, Class: ClassOf(in), Condition: in.Condition, Phase: in.Phase, Rerun: in.Rerun, RecordOnly: in.RecordOnly, Profile: slices.Clone(in.Profile), Outcome: end.Outcome, Signal: end.Signal, DurationS: end.DurationS, Core: end.Core})
}

func (s *Session) recordIdleFailure(e journal.Event, p *journal.Failure, build journal.Build, ids []int) {
	if p.KnownFailure == 0 && p.Trial == "" && (p.Condition == machine.Resident || p.Condition == machine.Masked) && p.Attribution == journal.Unattributed && len(p.Profile) == len(ids) {
		idle := *p
		idle.Profile = slices.Clone(p.Profile)
		s.Facts = append(s.Facts, Fact{Kind: IdleFact, Session: s.ID, Seq: e.Seq, Time: e.Time, Build: build, Ruleset: build.Ruleset, Epoch: s.Epoch, Boot: e.Boot, Class: Class{Regime: machine.R6, Cores: slices.Clone(ids)}, Condition: p.Condition, Profile: slices.Clone(p.Profile), Outcome: journal.OutcomeFailure, Signal: p.Signal, Core: p.Core, Idle: &idle})
	}
}

func (s *Session) recordResetEffects(e journal.Event, resetBySeq map[int]int) {
	for _, cause := range e.Cause {
		if i, ok := resetBySeq[cause]; ok {
			r := &s.Resets[i]
			r.Effects = append(r.Effects, e)
			if p, ok := e.Data.(*journal.CorePhase); ok && r.Core != nil && p.Core == *r.Core {
				r.AppliedSeq = e.Seq
			}
		}
	}
	if _, ok := e.Data.(*journal.SessionArchived); !ok {
		return
	}
	for i := range s.Resets {
		r := &s.Resets[i]
		if r.All && r.AppliedSeq == 0 {
			r.AppliedSeq = e.Seq
			if !slices.ContainsFunc(r.Effects, func(effect journal.Event) bool { return effect.Seq == e.Seq }) {
				r.Effects = append(r.Effects, e)
			}
		}
	}
}

func normalizedBuild(b journal.Build) journal.Build {
	if b.Ruleset == 0 {
		b.Ruleset = 1
	}
	// Epoch belongs to the session and Fact.Epoch, not the persisted binary stamp.
	b.EvidenceEpoch = 0
	return b
}

func ClassOf(p *journal.TrialIntent) Class {
	cores := slices.Clone(p.Cores)
	if p.Core != nil {
		cores = []int{*p.Core}
	}
	slices.Sort(cores)
	return Class{Regime: p.Regime, Workload: p.Workload, Cores: cores, DurationS: p.DurationS}
}

func trialProfile(p *journal.TrialIntent, ids, applied []int) []int {
	if len(p.Profile) == len(ids) {
		return slices.Clone(p.Profile)
	}
	profile := make([]int, len(ids))
	if p.Condition == machine.Isolated && p.Core != nil && p.Offset != nil {
		if i := slices.Index(ids, *p.Core); i >= 0 {
			profile[i] = *p.Offset
		}
	} else {
		copy(profile, applied)
	}
	return profile
}

func trialEvidence(t *Trial, e journal.Event) {
	if t != nil && t.Started && t.Boot == e.Boot && t.End == nil {
		t.LastEvidence = e.Time
	}
}

func (f Fact) Payload() journal.Payload {
	source := journal.FactSource{Session: f.Session, Seq: f.Seq, Build: f.Build, Trial: f.Trial, Time: f.Time, Boot: f.Boot, Evidence: f.Epoch}
	class := f.Class
	class.Cores = slices.Clone(class.Cores)
	slices.Sort(class.Cores)
	if f.Kind == IdleFact {
		idle := *f.Idle
		idle.Profile = slices.Clone(f.Profile)
		return &journal.FailureCarried{Source: source, Class: class, Failure: idle}
	}
	return &journal.TrialCarried{Source: source, Class: class, Condition: f.Condition, Phase: f.Phase, Rerun: f.Rerun, RecordOnly: f.RecordOnly, Profile: slices.Clone(f.Profile), Outcome: f.Outcome, Signal: f.Signal, DurationS: f.DurationS, Core: f.Core}
}

func carriedTrial(p *journal.TrialCarried) Fact {
	f := sourceFact(p.Source, p.Class)
	f.Kind, f.Condition, f.Phase, f.Rerun, f.RecordOnly = TrialFact, p.Condition, p.Phase, p.Rerun, p.RecordOnly
	f.Profile, f.Outcome, f.Signal, f.DurationS, f.Core = slices.Clone(p.Profile), p.Outcome, p.Signal, p.DurationS, p.Core
	return f
}

func carriedFailure(p *journal.FailureCarried) Fact {
	f := sourceFact(p.Source, p.Class)
	idle := p.Failure
	idle.Profile = slices.Clone(p.Profile)
	f.Kind, f.Condition, f.Profile, f.Outcome, f.Signal, f.Core, f.Idle = IdleFact, p.Condition, slices.Clone(p.Profile), journal.OutcomeFailure, p.Signal, p.Core, &idle
	return f
}

func sourceFact(source journal.FactSource, class Class) Fact {
	class.Cores = slices.Clone(class.Cores)
	slices.Sort(class.Cores)
	return Fact{Session: source.Session, Seq: source.Seq, Time: source.Time, Build: source.Build, Ruleset: source.Build.Ruleset, Epoch: source.Evidence, Trial: source.Trial, Boot: source.Boot, Class: class}
}
