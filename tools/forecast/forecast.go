// Package forecast defines a forecast of a real run from a copy of its state directory: the anchor the forecast starts
// after, the outcome of a run after that anchor, the summary of a simulated ensemble and the score of the real run
// against it. tools/bench writes forecasts and tools/stats scores them, both through this package.
package forecast

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"slices"
	"time"

	"github.com/shgew/togi/internal/journal"
	togisession "github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/tuner"
)

// Run statuses after the anchor.
const (
	// Concluded: the run reached the clean cycle `togi run --cycles 1` stops at.
	Concluded = "concluded"
	// DeadEnd: the run stopped at a dead end before concluding.
	DeadEnd = "deadend"
	// Censored: the run had not concluded when its journal ends.
	Censored = "censored"
)

var (
	// ErrMissingAnchor reports a state directory that does not hold the anchor event.
	ErrMissingAnchor = errors.New("anchor not found")
	// ErrChangedAnchor reports a state directory whose journal through the anchor differs from the forecast's copy.
	ErrChangedAnchor = errors.New("anchor changed")
)

// RecoveryBias states how simulated hours differ from real ones. Forecasts report simulated hours as they are.
func RecoveryBias() string {
	return fmt.Sprintf("hours are simulated; a simulated crash reboot takes %.0f s, while real recovery took a median of 141 s on 2026-10-03 (about 37 minutes over that session), so real runs take longer; not corrected", sim.RebootTime.Seconds())
}

// Anchor is the last event of the copied live journal; a forecast describes only what follows it.
type Anchor struct {
	Session string    `json:"session"`
	Seq     int       `json:"seq"`
	Time    time.Time `json:"time"`
	// SHA256 hashes the anchor session's journal lines through the anchor event.
	SHA256 string `json:"sha256"`
}

// Outcome is what a run did after the anchor, through its first terminal point: a conclusion or a dead end. Events
// after that point are ignored.
type Outcome struct {
	Status string `json:"status"`
	// Hours runs from the anchor to the terminal point, or to the last event of a run that has none.
	Hours float64 `json:"hours"`
	// Crashes counts crash.detected events.
	Crashes int `json:"crashes"`
	// Hunts counts hunts that ran a trial; hunts answered by carried facts alone run none.
	Hunts int `json:"hunts"`
	// Profile is the profile, by core, of the session holding the terminal point, or of the newest session.
	Profile []int `json:"profile"`
	Depth   int   `json:"depth"`
}

// Session is one session's journal.
type Session struct {
	ID     string
	Events []journal.Event
	// Replayable is true when the journal was read with its configuration and records this build's ruleset, so the
	// tuner can replay it.
	Replayable bool
}

// ReadDir reads the archived and live journals of a state directory, oldest session first, skipping archived sessions
// older than from. It never writes.
func ReadDir(dir, from string) ([]Session, error) {
	ids, err := journal.ArchivedSessions(dir)
	if err != nil {
		return nil, err
	}
	var sessions []Session
	for _, id := range ids {
		if from != "" && journal.CompareSessionIDs(id, from) < 0 {
			continue
		}
		s, err := readSession(filepath.Join(dir, "archive", id+".jsonl"))
		if err != nil {
			return nil, err
		}
		if s.ID == "" {
			s.ID = id
		}
		sessions = append(sessions, s)
	}
	live, err := readSession(filepath.Join(dir, "events.jsonl"))
	if errors.Is(err, fs.ErrNotExist) {
		return sessions, nil
	}
	if err != nil {
		return nil, err
	}
	if len(live.Events) > 0 {
		sessions = append(sessions, live)
	}
	return sessions, nil
}

func readSession(path string) (Session, error) {
	events, _, err := journal.ReadFile(path)
	replayable := err == nil
	if err != nil {
		var historyErr error
		if events, historyErr = journal.ReadHistory(path); historyErr != nil {
			return Session{}, err
		}
	}
	s := Session{Events: events, Replayable: replayable && journal.Classify(journal.BuildOf(events), togisession.Build()).Ruleset == journal.DirSame}
	if len(events) > 0 {
		if start, ok := events[0].Data.(*journal.SessionStart); ok {
			s.ID = start.Session
		}
	}
	return s, nil
}

// AnchorOf returns the anchor of a copied state directory: the last complete event of its live journal.
func AnchorOf(dir string) (Anchor, error) {
	events, err := journal.ReadHistory(filepath.Join(dir, "events.jsonl"))
	if err != nil {
		return Anchor{}, err
	}
	if len(events) == 0 {
		return Anchor{}, fmt.Errorf("live journal in %s has no events", dir)
	}
	start, ok := events[0].Data.(*journal.SessionStart)
	if !ok {
		return Anchor{}, fmt.Errorf("live journal in %s does not open with session.start", dir)
	}
	last := events[len(events)-1]
	return Anchor{Session: start.Session, Seq: last.Seq, Time: last.Time.UTC(), SHA256: hashThrough(events, len(events)-1)}, nil
}

func hashThrough(events []journal.Event, last int) string {
	h := sha256.New()
	for _, e := range events[:last+1] {
		h.Write(e.Raw)
		h.Write([]byte{'\n'})
	}
	return hex.EncodeToString(h.Sum(nil))
}

// find returns the index of the anchor session and of the anchor event in it, checking the journal through the anchor.
func (a Anchor) find(sessions []Session) (int, int, error) {
	for i, s := range sessions {
		if s.ID != a.Session {
			continue
		}
		for j, e := range s.Events {
			if e.Seq != a.Seq {
				continue
			}
			if hashThrough(s.Events, j) != a.SHA256 {
				return 0, 0, fmt.Errorf("%w: session %s through event %d differs from the forecast's copy", ErrChangedAnchor, a.Session, a.Seq)
			}
			return i, j, nil
		}
		return 0, 0, fmt.Errorf("%w: session %s has no event %d", ErrMissingAnchor, a.Session, a.Seq)
	}
	return 0, 0, fmt.Errorf("%w: no journal holds session %s", ErrMissingAnchor, a.Session)
}

// After returns what the sessions record after the anchor, through the first terminal point: a checking.cycle start
// after a clean cycle, where `togi run --cycles 1` shuts down instead (not counted), a shutdown with reason cycles, or
// a dead end. The session holding that point, or the newest session when there is none, must be replayable, so the
// tuner can project its profile.
func After(sessions []Session, a Anchor) (Outcome, error) {
	o, _, err := after(sessions, a)
	return o, err
}

// after returns the outcome After returns and the distinct builds that ran in it, in order: the build in effect at
// the anchor when its invocation wrote events after the anchor, then each build stamped after the anchor through the
// terminal point.
func after(sessions []Session, a Anchor) (Outcome, []journal.Build, error) {
	first, at, err := a.find(sessions)
	if err != nil {
		return Outcome{}, nil, err
	}
	sc := scan{o: Outcome{Status: Censored}, last: a.Time, hunts: map[huntKey]bool{}}
	var held Session
	var t *tuner.State
	var st journal.State
walk:
	for i, s := range sessions[first:] {
		start := 0
		if i == 0 {
			start = at + 1
		}
		held, t, st = s, tuner.New(), journal.State{}
		ruleset := journal.BuildOf(s.Events).Ruleset
		var inEffect *journal.Build
		for j, e := range s.Events {
			end, fold := false, true
			if j < start {
				if b, ok := stampOf(e, ruleset); ok {
					inEffect = &b
				}
			} else {
				sc.stamp(e, ruleset, inEffect)
				inEffect = nil
				end, fold = sc.event(s, e, t)
			}
			if fold && s.Replayable {
				st.Fold(e)
				t.Fold(e)
			}
			if end {
				break walk
			}
		}
	}
	if !held.Replayable {
		return Outcome{}, nil, fmt.Errorf("session %s, ruleset %d, cannot be replayed by this build's ruleset %d", held.ID, journal.BuildOf(held.Events).Ruleset, tuner.Ruleset)
	}
	o := sc.o
	o.Profile = profileOf(t, &st)
	for _, offset := range o.Profile {
		o.Depth += offset
	}
	o.Hunts = len(sc.hunts)
	o.Hours = sc.last.Sub(a.Time).Hours()
	return o, sc.builds, nil
}

type huntKey struct {
	session string
	hunt    int
}

// scan accumulates a run's outcome and builds event by event after the anchor.
type scan struct {
	o      Outcome
	last   time.Time
	builds []journal.Build
	hunts  map[huntKey]bool
}

// stamp records the build e stamps or, when it stamps none, the build in effect at the anchor.
func (sc *scan) stamp(e journal.Event, ruleset int, inEffect *journal.Build) {
	b, ok := stampOf(e, ruleset)
	if !ok {
		if inEffect == nil {
			return
		}
		b = *inEffect
	}
	if !slices.Contains(sc.builds, b) {
		sc.builds = append(sc.builds, b)
	}
}

// event counts e from session s, whose tuner t has folded the events before it, and reports whether e is the
// terminal point and whether it is folded.
func (sc *scan) event(s Session, e journal.Event, t *tuner.State) (end, fold bool) {
	switch p := e.Data.(type) {
	case *journal.CheckingCycle:
		if s.Replayable && p.Event == journal.CycleStart && t.CleanCycles() >= 1 {
			sc.o.Status, sc.last = Concluded, e.Time
			return true, false
		}
	case *journal.Shutdown:
		if p.Reason == journal.ShutdownCycles {
			sc.o.Status, end = Concluded, true
		}
	case *journal.CrashDetected:
		sc.o.Crashes++
	case *journal.TrialIntent:
		if p.Hunt > 0 {
			sc.hunts[huntKey{s.ID, p.Hunt}] = true
		}
	case *journal.DeadEnd:
		sc.o.Status, end = DeadEnd, true
	}
	sc.last = e.Time
	return end, true
}

// profileOf projects the folded session's profile, falling back to its baseline.
func profileOf(t *tuner.State, st *journal.State) []int {
	t.Project(st)
	if profile := t.Profile(); len(profile) > 0 {
		return profile
	}
	profile := make([]int, len(st.Cores))
	if st.Session != nil {
		copy(profile, st.Session.Baseline)
	}
	return profile
}

// File is a machine file or facts extract the ensemble read, with its content hash.
type File struct {
	Path   string `json:"path"`
	SHA256 string `json:"sha256"`
}

// Run is one simulated run of the ensemble.
type Run struct {
	Machine string `json:"machine"`
	Seed    uint64 `json:"seed"`
	Split   string `json:"split"`
	Outcome
}

// Record is a committed forecast.
type Record struct {
	Anchor  Anchor  `json:"anchor"`
	Commit  string  `json:"commit"`
	Dirty   bool    `json:"dirty"`
	Ruleset int     `json:"ruleset"`
	Files   []File  `json:"files"`
	Runs    []Run   `json:"runs"`
	Summary Summary `json:"summary"`
}

// Write encodes the record as indented JSON.
func (r Record) Write(w io.Writer) error {
	encoder := json.NewEncoder(w)
	encoder.SetIndent("", "  ")
	return encoder.Encode(r)
}

// ReadRecord decodes a committed forecast.
func ReadRecord(path string) (Record, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return Record{}, fmt.Errorf("read forecast: %w", err)
	}
	var r Record
	if err := json.Unmarshal(data, &r); err != nil {
		return Record{}, fmt.Errorf("decode forecast %s: %w", path, err)
	}
	if len(r.Runs) == 0 {
		return Record{}, fmt.Errorf("forecast %s has no runs", path)
	}
	return r, nil
}
