// Package carry derives what a session written by an older ruleset or schema carries into the next one: each core's
// deepest isolated pass as a candidate edge, and its shallowest attributed failure as a failed mark.
package carry

import (
	"errors"
	"fmt"
	"io/fs"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strings"

	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type Carry struct {
	Sources []journal.CarriedSource
	Context *machine.BIOSContext  // the BIOS context Sources[0] recorded; nil if it recorded none
	Cores   []journal.CarriedCore // ascending core; each has an Edge, a FailedMark, or both
}

// Prepare readies dir for a session of binary: a journal an older ruleset or schema wrote is archived, and the carry of
// the session a transition archived is returned until a journal records it. A nil entries uses defect.Entries().
func Prepare(dir string, opts journal.Options, binary journal.Build, entries []defect.Entry) (*Carry, error) {
	stamp, id, err := journal.Scan(dir)
	switch {
	case errors.Is(err, fs.ErrNotExist):
	case err != nil:
		return nil, fmt.Errorf("carry: %w", err)
	case stamp.Schema == 0:
	case journal.Older(stamp, binary):
		j, err := journal.OpenForArchive(dir, journal.Options{Boot: opts.Boot, Now: opts.Now, Sync: opts.Sync})
		if err != nil {
			return nil, fmt.Errorf("carry: archive session %s: %w", id, err)
		}
		_, err = j.ArchiveForCarry(id)
		if closeErr := j.Close(); err == nil {
			err = closeErr
		}
		if err != nil {
			return nil, fmt.Errorf("carry: archive session %s: %w", id, err)
		}
	case journal.Compatible(stamp, binary) != nil:
		return nil, nil
	}
	pending, err := journal.PendingCarry(dir)
	if err != nil {
		return nil, fmt.Errorf("carry: %w", err)
	}
	if pending == "" {
		return nil, nil
	}
	settled, err := recorded(dir, pending)
	if err != nil {
		return nil, fmt.Errorf("carry: %w", err)
	}
	if settled {
		if err := journal.ClearPendingCarry(dir); err != nil {
			return nil, fmt.Errorf("carry: %w", err)
		}
		return nil, nil
	}
	if entries == nil {
		entries = defect.Entries()
	}
	return compute(dir, pending, entries)
}

// recorded reports whether the current journal has recorded the carry of session id, or is past the point where one
// can apply.
func recorded(dir, id string) (bool, error) {
	if _, err := os.Stat(filepath.Join(dir, "events.jsonl")); errors.Is(err, fs.ErrNotExist) {
		return false, nil
	}
	events, _, err := journal.Read(dir)
	if err != nil {
		return false, err
	}
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.SessionCarried:
			if len(p.Sources) > 0 && p.Sources[0].Session == id {
				return true, nil
			}
		case *journal.CorePhase:
			return true, nil
		}
	}
	return false, nil
}

type source struct {
	journal.CarriedSource
	context *machine.BIOSContext
	seeded  bool
	events  []journal.Event
}

func read(dir, id string) (source, error) {
	path := filepath.Join("archive", id+".jsonl")
	events, err := journal.ReadForCarry(filepath.Join(dir, path))
	if err != nil {
		return source{}, fmt.Errorf("carry: read archived session %s: %w", id, err)
	}
	start := events[0].Data.(*journal.SessionStart)
	s := source{Session: start.Session, Path: path, Schema: start.Schema, Ruleset: max(start.Ruleset, 1), events: events}
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.SessionContext:
			s.context = &p.BIOSContext
		case *journal.SessionCarried:
			s.seeded = true
		}
	}
	return s, nil
}

func compute(dir, id string, entries []defect.Entry) (*Carry, error) {
	first, err := read(dir, id)
	if err != nil {
		return nil, err
	}
	sources := []source{first}
	if !first.seeded && first.context != nil {
		older, err := olderArchives(dir, id)
		if err != nil {
			return nil, err
		}
		for _, name := range older {
			s, err := read(dir, name)
			if err != nil {
				return nil, err
			}
			if s.context == nil || *s.context != *first.context || s.Ruleset == sources[len(sources)-1].Ruleset {
				break
			}
			sources = append(sources, s)
			if s.seeded {
				break
			}
		}
	}
	c := &Carry{Context: first.context}
	cores := make(map[int]*journal.CarriedCore)
	for _, s := range sources {
		c.Sources = append(c.Sources, s.CarriedSource)
		for _, v := range s.candidates(entries) {
			cc, ok := cores[v.core]
			if !ok {
				cc = &journal.CarriedCore{Core: v.core}
				cores[v.core] = cc
			}
			if v.edge {
				if cc.Edge == nil || v.offset < *cc.Edge {
					cc.Edge, cc.EdgeSession, cc.EdgeSeq = new(v.offset), v.session, v.seq
				}
				continue
			}
			if cc.FailedMark == nil || v.offset > *cc.FailedMark {
				cc.FailedMark, cc.MarkSession, cc.MarkSeq, cc.MarkSignal = new(v.offset), v.session, v.seq, v.signal
			}
		}
	}
	for _, core := range slices.Sorted(maps.Keys(cores)) {
		c.Cores = append(c.Cores, *cores[core])
	}
	return c, nil
}

// olderArchives lists the archived sessions older than id, newest first.
func olderArchives(dir, id string) ([]string, error) {
	paths, err := filepath.Glob(filepath.Join(dir, "archive", "*.jsonl"))
	if err != nil {
		return nil, fmt.Errorf("carry: list archives: %w", err)
	}
	var names []string
	for _, p := range paths {
		if name := strings.TrimSuffix(filepath.Base(p), ".jsonl"); name < id {
			names = append(names, name)
		}
	}
	slices.Sort(names)
	slices.Reverse(names)
	return names, nil
}

type candidate struct {
	core, offset int
	edge         bool
	session      string
	seq          int
	signal       machine.Signal
	// at is the seq a reset of the core must precede for the candidate to count.
	at int
}

// candidates lists, in seq order, the edges and marks the source's events yield, dropping those a later reset of their
// core cleared.
func (s source) candidates(entries []defect.Entry) []candidate {
	resetAt := make(map[int]int)
	excluded := defect.FailuresWith(s.events, entries)
	intents := make(map[string]*journal.TrialIntent)
	ended := make(map[string]bool)
	lastShutdown := 0
	for _, e := range s.events {
		switch p := e.Data.(type) {
		case *journal.CommandReset:
			if p.Core != nil {
				resetAt[*p.Core] = e.Seq
			}
		case *journal.TrialIntent:
			intents[p.Trial] = p
		case *journal.TrialEnd:
			ended[p.Trial] = true
		case *journal.Shutdown:
			lastShutdown = e.Seq
		}
	}
	var all []candidate
	var inFlight *journal.Event
	for i, e := range s.events {
		switch p := e.Data.(type) {
		case *journal.TrialEnd:
			in := intents[p.Trial]
			if p.Outcome == journal.OutcomePass && in != nil && in.Condition == machine.Isolated && in.Core != nil && in.Offset != nil {
				all = append(all, candidate{core: *in.Core, offset: *in.Offset, edge: true, session: s.Session, seq: e.Seq, at: e.Seq})
			}
		case *journal.Failure:
			if p.Attribution == journal.Attributed && p.Core != nil && p.Offset != nil && !slices.Contains(excluded, e.Seq) {
				all = append(all, candidate{core: *p.Core, offset: *p.Offset, session: s.Session, seq: e.Seq, signal: p.Signal, at: e.Seq})
			}
		case *journal.SessionCarried:
			for _, cc := range p.Carried {
				if cc.FailedMark != nil {
					all = append(all, candidate{core: cc.Core, offset: *cc.FailedMark, session: cc.MarkSession, seq: cc.MarkSeq, signal: cc.MarkSignal, at: e.Seq})
				}
				if cc.Edge != nil {
					all = append(all, candidate{core: cc.Core, offset: *cc.Edge, edge: true, session: cc.EdgeSession, seq: cc.EdgeSeq, at: e.Seq})
				}
			}
		case *journal.TrialIntent:
			if !ended[p.Trial] && e.Seq > lastShutdown {
				inFlight = &s.events[i]
			}
		}
	}
	if inFlight != nil {
		p := inFlight.Data.(*journal.TrialIntent)
		if p.Condition == machine.Isolated && p.Core != nil && p.Offset != nil && *p.Offset != 0 {
			all = append(all, candidate{core: *p.Core, offset: *p.Offset, session: s.Session, seq: inFlight.Seq, signal: machine.Crash, at: inFlight.Seq})
		}
	}
	return slices.DeleteFunc(all, func(v candidate) bool { return v.at <= resetAt[v.core] })
}
