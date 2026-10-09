package carry

import (
	"errors"
	"io/fs"
	"path/filepath"
	"slices"

	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type factID struct {
	session string
	seq     int
}

type factExclusions struct {
	seqs   []int
	trials map[string]bool
}

func excludeFacts(events []journal.Event, entries []defect.Entry) factExclusions {
	excluded := factExclusions{seqs: defect.FailuresWith(events, entries), trials: make(map[string]bool)}
	for _, e := range events {
		if p, ok := e.Data.(*journal.Failure); ok && p.Trial != "" && slices.Contains(excluded.seqs, e.Seq) {
			excluded.trials[p.Trial] = true
		}
	}
	return excluded
}

type factDefects struct {
	dir     string
	entries []defect.Entry
	sources map[string]factExclusions
}

func (d *factDefects) excludes(f facts.Fact) (bool, error) {
	if f.Outcome != journal.OutcomeFailure {
		return false, nil
	}
	excluded, ok := d.sources[f.Session]
	if !ok {
		source, err := read(d.dir, f.Session)
		if err != nil && !errors.Is(err, fs.ErrNotExist) {
			return false, err
		}
		if err == nil {
			excluded = excludeFacts(source.events, d.entries)
		}
		d.sources[f.Session] = excluded
	}
	return slices.Contains(excluded.seqs, f.Seq) || excluded.trials[f.Trial], nil
}

func prepareFacts(dir, id string, entries []defect.Entry, current *machine.BIOSContext, epoch int) ([]facts.Fact, error) {
	boundary, err := journal.ResetBoundary(dir)
	if err != nil {
		return nil, err
	}
	if boundary != "" && journal.CompareSessionIDs(id, boundary) <= 0 {
		return nil, nil
	}
	first, err := read(dir, id)
	if err != nil {
		return nil, err
	}
	if !sameFactContext(first.context, current) {
		return nil, nil
	}
	older, err := olderArchives(dir, id)
	if err != nil {
		return nil, err
	}
	cleared := make(map[int]bool)
	seen := make(map[factID]bool)
	defects := factDefects{dir: dir, entries: entries, sources: make(map[string]factExclusions)}
	var carried []facts.Fact
	sessions := make(map[string]facts.Session)
	source := first
	for i := 0; ; i++ {
		s := factSession(source.events)
		s.session.ReadRequests(filepath.Join(dir, "archive", s.session.ID+"-trials"))
		sessions[s.session.ID] = s.session
		defects.sources[s.session.ID] = excludeFacts(source.events, entries)
		carried, err = s.appendEligible(carried, seen, cleared, epoch, boundary, &defects)
		if err != nil {
			return nil, err
		}
		if s.allReset != 0 || len(s.session.Carried) != 0 && source.seeded || i == len(older) {
			break
		}
		for core := range s.coreResets {
			cleared[core] = true
		}
		if boundary != "" && journal.CompareSessionIDs(older[i], boundary) <= 0 {
			break
		}
		source, err = read(dir, older[i])
		if err != nil {
			return nil, err
		}
		if !sameFactContext(source.context, first.context) {
			break
		}
	}
	if err := fillFromOriginals(dir, carried, sessions); err != nil {
		return nil, err
	}
	slices.SortFunc(carried, func(a, b facts.Fact) int {
		if order := journal.CompareSessionIDs(a.Session, b.Session); order != 0 {
			return order
		}
		return a.Seq - b.Seq
	})
	return carried, nil
}

// fillFromOriginals copies, from each carried fact's original session, request telemetry the fact lacks and a pass's
// backend identity. The original is a walked session, or its archive when that survives.
func fillFromOriginals(dir string, carried []facts.Fact, sessions map[string]facts.Session) error {
	originals := make(map[string]map[int]facts.Fact)
	for i := range carried {
		f := &carried[i]
		needsRequests := f.Kind == facts.TrialFact && len(f.VoltageRequestsV) == 0 && len(f.TopRequesters) == 0 && len(f.CCDMHz) == 0
		if !needsRequests && f.Outcome != journal.OutcomePass {
			continue
		}
		bySeq, ok := originals[f.Session]
		if !ok {
			original, err := originalSession(dir, f.Session, sessions)
			if err != nil {
				return err
			}
			bySeq = make(map[int]facts.Fact, len(original.Facts))
			for _, o := range original.Facts {
				bySeq[o.Seq] = o
			}
			originals[f.Session] = bySeq
		}
		source, ok := bySeq[f.Seq]
		if !ok {
			continue
		}
		if needsRequests {
			f.VoltageRequestsV, f.TopRequesters, f.CCDMHz = source.VoltageRequestsV, source.TopRequesters, source.CCDMHz
			if f.StalledCore == nil {
				f.StalledCore = source.StalledCore
			}
		}
		f.Backend = source.Backend
	}
	return nil
}

// originalSession returns a walked session, or reads the archived one; a missing archive yields no facts.
func originalSession(dir, id string, sessions map[string]facts.Session) (facts.Session, error) {
	if s, walked := sessions[id]; walked {
		return s, nil
	}
	source, err := read(dir, id)
	if errors.Is(err, fs.ErrNotExist) {
		return facts.Session{}, nil
	}
	if err != nil {
		return facts.Session{}, err
	}
	original := facts.FromEvents(source.events)
	original.ReadRequests(filepath.Join(dir, "archive", id+"-trials"))
	return original, nil
}

func sameFactContext(recorded, current *machine.BIOSContext) bool {
	if recorded == nil || current == nil {
		return false
	}
	_, same := machine.CompareContext(*recorded, *current)
	return same
}

type sessionFacts struct {
	session    facts.Session
	allReset   int
	coreResets map[int]int
	positions  map[factID]int
}

func factSession(events []journal.Event) sessionFacts {
	s := sessionFacts{
		session:    facts.FromEvents(events),
		coreResets: make(map[int]int),
		positions:  make(map[factID]int),
	}
	for _, reset := range s.session.Resets {
		if reset.All {
			s.allReset = max(s.allReset, reset.Seq)
		}
		if reset.Core != nil {
			s.coreResets[*reset.Core] = max(s.coreResets[*reset.Core], reset.Seq)
		}
	}
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.TrialCarried:
			s.positions[factID{p.Source.Session, p.Source.Seq}] = e.Seq
		case *journal.FailureCarried:
			s.positions[factID{p.Source.Session, p.Source.Seq}] = e.Seq
		}
	}
	return s
}

func (s sessionFacts) eligible(f facts.Fact, cleared map[int]bool, epoch int) bool {
	at := f.Seq
	if position, ok := s.positions[factID{f.Session, f.Seq}]; ok {
		at = position
	}
	sameEpoch := journal.Classify(journal.Build{EvidenceEpoch: f.Epoch}, journal.Build{EvidenceEpoch: epoch}).Epoch == journal.DirSame
	if at <= s.allReset || f.Outcome == journal.OutcomePass && !sameEpoch {
		return false
	}
	return !slices.ContainsFunc(f.Class.Cores, func(core int) bool { return cleared[core] || at <= s.coreResets[core] })
}

func (s sessionFacts) appendEligible(carried []facts.Fact, seen map[factID]bool, cleared map[int]bool, epoch int, boundary string, defects *factDefects) ([]facts.Fact, error) {
	for _, group := range [][]facts.Fact{s.session.Facts, s.session.Carried} {
		for _, f := range group {
			if boundary != "" && journal.CompareSessionIDs(f.Session, boundary) <= 0 {
				continue
			}
			key := factID{f.Session, f.Seq}
			if seen[key] || !s.eligible(f, cleared, epoch) {
				continue
			}
			excluded, err := defects.excludes(f)
			if err != nil {
				return nil, err
			}
			if excluded {
				continue
			}
			seen[key] = true
			carried = append(carried, f)
		}
	}
	return carried, nil
}
