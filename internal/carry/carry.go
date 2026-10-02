// Package carry derives the candidate edges, failed marks and trial facts a
// session written by an older ruleset, schema or evidence epoch carries into the next one.
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
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type Carry struct {
	Sources []journal.CarriedSource
	Context *machine.BIOSContext  // the BIOS context Sources[0] recorded; nil if it recorded none
	Cores   []journal.CarriedCore // ascending core; each has an Edge, a FailedMark, or both
	Facts   []facts.Fact

	factDir     string
	factEntries []defect.Entry
	factEpoch   int
}

func Prepare(j *journal.Journal, binary journal.Build, entries []defect.Entry, current *machine.BIOSContext) (*Carry, error) {
	dir := j.Dir()
	boundary, boundaryErr := journal.ResetBoundary(dir)
	if boundaryErr != nil {
		return nil, fmt.Errorf("carry: %w", boundaryErr)
	}
	stamp, id, err := journal.Scan(dir)
	if err == nil && id != "" && boundary != "" && journal.CompareSessionIDs(id, boundary) <= 0 {
		if _, archiveErr := j.ArchiveUnreadable(id); archiveErr != nil {
			return nil, fmt.Errorf("carry: finish reset archive: %w", archiveErr)
		}
		if clearErr := j.ClearPendingCarry(); clearErr != nil {
			return nil, fmt.Errorf("carry: finish reset carry removal: %w", clearErr)
		}
		return nil, nil
	}
	events, _, readErr := journal.Read(dir)
	if readErr == nil && !journal.Older(journal.BuildOf(events), binary) {
		if err := journal.KnownKinds(events, binary); err != nil {
			return nil, err
		}
	}
	switch {
	case errors.Is(err, fs.ErrNotExist):
	case err != nil:
		return nil, fmt.Errorf("carry: %w", err)
	case stamp.Schema == binary.Schema && readErr != nil:
		return nil, fmt.Errorf("carry: %w", readErr)
	case stamp.Schema == 0:
	case journal.Older(stamp, binary):
		if err := archive(j, id); err != nil {
			return nil, err
		}
	case journal.Compatible(stamp, binary) == nil && current != nil:
		recorded, err := journal.RecordedContext(dir)
		if err != nil {
			return nil, fmt.Errorf("carry: read recorded BIOS context: %w", err)
		}
		if recorded != nil {
			if _, same := machine.CompareContext(*recorded, *current); !same {
				if err := archive(j, id); err != nil {
					return nil, err
				}
			}
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
	if boundary != "" && journal.CompareSessionIDs(pending, boundary) <= 0 {
		if err := j.ClearPendingCarry(); err != nil {
			return nil, fmt.Errorf("carry: %w", err)
		}
		return nil, nil
	}
	settled, err := recorded(dir, pending)
	if err != nil {
		return nil, fmt.Errorf("carry: %w", err)
	}
	if settled {
		if err := j.ClearPendingCarry(); err != nil {
			return nil, fmt.Errorf("carry: %w", err)
		}
		return nil, nil
	}
	if entries == nil {
		entries = defect.Entries()
	}
	c, err := compute(dir, pending, entries)
	if err != nil {
		return nil, err
	}
	if current == nil {
		c.factDir, c.factEntries, c.factEpoch = dir, entries, binary.Epoch()
	} else {
		c.Facts, err = prepareFacts(dir, pending, entries, current, binary.Epoch())
		if err != nil {
			return nil, err
		}
	}
	return c, nil
}

// ResolveFacts completes deferred eligibility after the session validates its actual BIOS context.
func (c *Carry) ResolveFacts(current *machine.BIOSContext) error {
	if current == nil {
		return errors.New("carry: cannot prepare facts without the current BIOS context")
	}
	if c.factDir == "" {
		return nil
	}
	fs, err := prepareFacts(c.factDir, c.Sources[0].Session, c.factEntries, current, c.factEpoch)
	if err != nil {
		return err
	}
	c.Facts = fs
	c.factDir, c.factEntries = "", nil
	return nil
}

func archive(j *journal.Journal, id string) error {
	if _, err := j.ArchiveForCarry(id); err != nil {
		return fmt.Errorf("carry: archive session %s: %w", id, err)
	}
	return nil
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
	epoch   int
	events  []journal.Event
}

func read(dir, id string) (source, error) {
	path := filepath.Join("archive", id+".jsonl")
	events, err := journal.ReadForCarry(filepath.Join(dir, path))
	if err != nil {
		return source{}, fmt.Errorf("carry: read archived session %s: %w", id, err)
	}
	start := events[0].Data.(*journal.SessionStart)
	s := source{Session: start.Session, Path: path, Schema: start.Schema, Ruleset: max(start.Ruleset, 1), epoch: start.Epoch(), events: events}
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
	boundary, err := journal.ResetBoundary(dir)
	if err != nil {
		return nil, err
	}
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
			if boundary != "" && journal.CompareSessionIDs(name, boundary) <= 0 {
				break
			}
			s, err := read(dir, name)
			if err != nil {
				return nil, err
			}
			previous := sources[len(sources)-1]
			if s.context == nil || *s.context != *first.context || s.Ruleset == previous.Ruleset && s.epoch == previous.epoch {
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
			if boundary != "" && journal.CompareSessionIDs(v.session, boundary) <= 0 {
				continue
			}
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
		if name := strings.TrimSuffix(filepath.Base(p), ".jsonl"); journal.CompareSessionIDs(name, id) < 0 {
			names = append(names, name)
		}
	}
	slices.SortFunc(names, journal.CompareSessionIDs)
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
	hunts := make(map[int]*journal.HuntStart)
	signals := make(map[int]machine.Signal)
	var ids []int
	lastShutdown := 0
	for _, e := range s.events {
		switch p := e.Data.(type) {
		case *journal.SessionStart:
			ids = ids[:0]
			for _, c := range p.Cores {
				ids = append(ids, c.Core)
			}
			slices.Sort(ids)
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
		case *journal.HuntStart:
			hunts[p.Hunt] = p
		case *journal.Failure:
			signals[e.Seq] = p.Signal
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
			if p.KnownFailure == 0 && p.Attribution == journal.Attributed && p.Core != nil && p.Offset != nil && !slices.Contains(excluded, e.Seq) {
				all = append(all, candidate{core: *p.Core, offset: *p.Offset, session: s.Session, seq: e.Seq, signal: p.Signal, at: e.Seq})
			}
		case *journal.HuntEnd:
			if p.Result == "culprit" && len(p.Cores) == 1 {
				start := hunts[p.Hunt]
				core := p.Cores[0]
				i := slices.Index(ids, core)
				if start != nil && i >= 0 && i < len(start.Failing) {
					if signal, ok := signals[start.Failure]; ok {
						all = append(all, candidate{core: core, offset: start.Failing[i], session: s.Session, seq: e.Seq, signal: signal, at: e.Seq})
					}
				}
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
