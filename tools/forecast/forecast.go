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
	"time"

	"github.com/shgew/togi/internal/journal"
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

// Outcome is what a run did after the anchor.
type Outcome struct {
	Status string `json:"status"`
	// Hours runs from the anchor to the conclusion, or to the last event of a run that did not conclude.
	Hours float64 `json:"hours"`
	// Crashes counts crash.detected events.
	Crashes int `json:"crashes"`
	// Hunts counts hunts that ran a trial; hunts answered by carried facts alone run none.
	Hunts int `json:"hunts"`
	// Profile is the newest session's final profile, by core.
	Profile []int `json:"profile"`
	Depth   int   `json:"depth"`
}

// Session is one session's journal.
type Session struct {
	ID     string
	Events []journal.Event
	// Configured is true when the journal was read with its configuration, which needs this build's schema and
	// ruleset; only a configured session can be replayed through the tuner.
	Configured bool
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
	configured := err == nil
	if err != nil {
		var historyErr error
		if events, historyErr = journal.ReadHistory(path); historyErr != nil {
			return Session{}, err
		}
	}
	s := Session{Events: events, Configured: configured}
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

// After returns what the sessions record after the anchor. The newest session must be configured, so the tuner can
// project its final profile.
func After(sessions []Session, a Anchor) (Outcome, error) {
	first, at, err := a.find(sessions)
	if err != nil {
		return Outcome{}, err
	}
	o := Outcome{Status: Censored}
	last := a.Time
	var concluded *time.Time
	deadEnd := false
	type huntKey struct {
		session string
		hunt    int
	}
	hunts := map[huntKey]bool{}
	for i, s := range sessions[first:] {
		start := 0
		if i == 0 {
			start = at + 1
		}
		t := tuner.New()
		var st journal.State
		for j, e := range s.Events {
			if j >= start {
				last = e.Time
				switch p := e.Data.(type) {
				case *journal.CheckingCycle:
					if concluded == nil && s.Configured && p.Event == journal.CycleStart && t.CleanCycles() >= 1 {
						concluded = new(e.Time)
					}
				case *journal.Shutdown:
					if concluded == nil && p.Reason == journal.ShutdownCycles {
						concluded = new(e.Time)
					}
				case *journal.CrashDetected:
					o.Crashes++
				case *journal.TrialIntent:
					if p.Hunt > 0 {
						hunts[huntKey{s.ID, p.Hunt}] = true
					}
				case *journal.DeadEnd:
					deadEnd = true
				}
			}
			if s.Configured {
				st.Fold(e)
				t.Fold(e)
			}
		}
		if i == len(sessions[first:])-1 {
			if !s.Configured {
				return Outcome{}, fmt.Errorf("session %s, ruleset %d, cannot be replayed by this build's ruleset %d", s.ID, journal.BuildOf(s.Events).Ruleset, tuner.Ruleset)
			}
			t.Project(&st)
			o.Profile = t.Profile()
			if len(o.Profile) == 0 {
				o.Profile = make([]int, len(st.Cores))
				if st.Session != nil {
					copy(o.Profile, st.Session.Baseline)
				}
			}
		}
	}
	for _, offset := range o.Profile {
		o.Depth += offset
	}
	o.Hunts = len(hunts)
	switch {
	case deadEnd:
		o.Status = DeadEnd
	case concluded != nil:
		o.Status = Concluded
		last = *concluded
	}
	o.Hours = last.Sub(a.Time).Hours()
	return o, nil
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
