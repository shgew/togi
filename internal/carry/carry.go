// Package carry derives the candidate solo limits, failure points and trial facts a
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

	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type Carry struct {
	Sources []journal.CarriedSource
	Context *machine.BIOSContext  // the BIOS context Sources[0] recorded; nil if it recorded none
	Cores   []journal.CarriedCore // ascending core; each has a CandidateSoloLimit, a FailurePoint, or both
	Facts   []facts.Fact          // final only after ResolveFacts, which drops passes of other backend store paths

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
	events, _, readErr := j.Read()
	if readErr == nil {
		if err := journal.Classify(journal.BuildOf(events), binary).Kinds(journal.OpResume, events); err != nil {
			return nil, err
		}
	}
	compat := journal.Classify(stamp, binary)
	switch {
	case errors.Is(err, fs.ErrNotExist):
	case err != nil:
		return nil, fmt.Errorf("carry: %w", err)
	case compat.Schema == journal.DirSame && readErr != nil:
		return nil, fmt.Errorf("carry: %w", readErr)
	case stamp.Schema == 0:
	case compat.Access(journal.OpResume) == journal.AccessArchive:
		if err := archive(j, id); err != nil {
			return nil, err
		}
	case compat.Access(journal.OpResume) == journal.AccessUse && current != nil:
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
	case compat.Access(journal.OpResume) == journal.AccessRefuse:
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
	settled, err := recorded(j, pending)
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

// ResolveFacts completes deferred eligibility after the session validates its actual BIOS context, then keeps a pass
// only when it ran under the package store path backends, the session's recorded configuration, names for its backend.
// Failures stay whichever binary ran them.
func (c *Carry) ResolveFacts(current *machine.BIOSContext, backends journal.ConfigBackends) error {
	if current == nil {
		return errors.New("carry: cannot prepare facts without the current BIOS context")
	}
	if c.factDir != "" {
		fs, err := prepareFacts(c.factDir, c.Sources[0].Session, c.factEntries, current, c.factEpoch)
		if err != nil {
			return err
		}
		c.Facts = fs
		c.factDir, c.factEntries = "", nil
	}
	c.Facts = slices.DeleteFunc(c.Facts, func(f facts.Fact) bool {
		return f.Outcome == journal.OutcomePass && f.Backend != backends.WorkloadPath(f.Class.Workload)
	})
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
func recorded(j *journal.Journal, id string) (bool, error) {
	if _, err := os.Stat(filepath.Join(j.Dir(), "events.jsonl")); errors.Is(err, fs.ErrNotExist) {
		return false, nil
	}
	events, _, err := j.Read()
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
	zeros := zeroConfirmations{dir: dir, bySession: make(map[string]map[int]bool)}
	for _, s := range sources {
		zeros.bySession[s.Session] = confirmedZeros(s.events)
	}
	for _, s := range sources {
		c.Sources = append(c.Sources, s.CarriedSource)
		for _, v := range s.candidates(entries) {
			if boundary != "" && journal.CompareSessionIDs(v.session, boundary) <= 0 {
				continue
			}
			admitted, err := zeros.admits(v)
			if err != nil {
				return nil, err
			}
			if admitted {
				merge(cores, v)
			}
		}
	}
	for _, core := range slices.Sorted(maps.Keys(cores)) {
		c.Cores = append(c.Cores, *cores[core])
	}
	return c, nil
}

// merge keeps the deepest candidate solo limit and the shallowest failure point per core; on a tie, the first found.
func merge(cores map[int]*journal.CarriedCore, v candidate) {
	cc, ok := cores[v.core]
	if !ok {
		cc = &journal.CarriedCore{Core: v.core}
		cores[v.core] = cc
	}
	if v.candidateSoloLimit {
		if cc.CandidateSoloLimit == nil || v.offset < *cc.CandidateSoloLimit {
			cc.CandidateSoloLimit, cc.CandidateSoloLimitSession, cc.CandidateSoloLimitSeq = new(v.offset), v.session, v.seq
		}
		return
	}
	if cc.FailurePoint == nil || v.offset > *cc.FailurePoint {
		cc.FailurePoint, cc.FailurePointSession, cc.FailurePointSeq, cc.FailurePointSignal = new(v.offset), v.session, v.seq, v.signal
	}
}

// zeroConfirmations caches, per original session, which of its failures at CO 0 it confirmed.
type zeroConfirmations struct {
	dir       string
	bySession map[string]map[int]bool
}

// admits reports whether a candidate carries: every one but a failure point at CO 0 its session did not confirm.
func (z zeroConfirmations) admits(v candidate) (bool, error) {
	if v.candidateSoloLimit || v.offset != 0 {
		return true, nil
	}
	return z.confirmed(v.session, v.seq)
}

// confirmed reports whether the failure at CO 0 recorded at seq in session was confirmed there. A session whose archive
// is gone confirms nothing.
func (z zeroConfirmations) confirmed(session string, seq int) (bool, error) {
	seqs, ok := z.bySession[session]
	if !ok {
		events, err := journal.ReadForCarry(filepath.Join(z.dir, "archive", session+".jsonl"))
		switch {
		case errors.Is(err, fs.ErrNotExist):
		case err != nil:
			return false, fmt.Errorf("carry: read archived session %s: %w", session, err)
		default:
			seqs = confirmedZeros(events)
		}
		z.bySession[session] = seqs
	}
	return seqs[seq], nil
}

// confirmedZeros lists the failures at CO 0 a session confirmed, by the sequence a failure point cites: an attributed
// failure or a single-core hunt culprit whose failing profile was all at CO 0 (as a trial alone at 0 is), or whose
// all-zero rerun failed.
func confirmedZeros(events []journal.Event) map[int]bool {
	intents := make(map[string]*journal.TrialIntent)
	for _, t := range facts.FromEvents(events).Trials {
		intents[t.Intent.Trial] = t.Intent
	}
	reruns := make(map[string]int)
	rerunFailed := make(map[int]bool)
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.TrialIntent:
			if in := intents[p.Trial]; p.Condition == machine.Parked && p.Rerun && p.Hunt == 0 && len(e.Cause) > 0 && in != nil && allZero(in.Profile) {
				reruns[p.Trial] = e.Cause[0]
			}
		case *journal.TrialEnd:
			if failure, ok := reruns[p.Trial]; ok && p.Outcome == journal.OutcomeFailure {
				rerunFailed[failure] = true
			}
		}
	}
	var ids []int
	hunts := make(map[int]*journal.HuntStart)
	confirmed := make(map[int]bool)
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.SessionStart:
			ids = ids[:0]
			for _, c := range p.Cores {
				ids = append(ids, c.Core)
			}
		case *journal.Failure:
			if p.Offset == nil || *p.Offset != 0 {
				continue
			}
			in := intents[p.Trial]
			profile := p.Profile
			if len(profile) != len(ids) && in != nil {
				profile = in.Profile
			}
			alone := p.Condition == machine.Alone || in != nil && in.Condition == machine.Alone
			confirmed[e.Seq] = rerunFailed[e.Seq] || alone || allZero(profile)
		case *journal.HuntStart:
			hunts[p.Hunt] = p
		case *journal.HuntEnd:
			if start := hunts[p.Hunt]; start != nil && p.Result == "culprit" && len(p.Cores) == 1 {
				confirmed[e.Seq] = rerunFailed[start.Failure] || allZero(start.Failing)
			}
		}
	}
	return confirmed
}

func allZero(profile []int) bool {
	return len(profile) > 0 && !slices.ContainsFunc(profile, func(o int) bool { return o != 0 })
}

// olderArchives lists the archived sessions older than id, newest first.
func olderArchives(dir, id string) ([]string, error) {
	sessions, err := journal.ArchivedSessions(dir)
	if err != nil {
		return nil, fmt.Errorf("carry: %w", err)
	}
	var names []string
	for _, name := range slices.Backward(sessions) {
		if journal.CompareSessionIDs(name, id) < 0 {
			names = append(names, name)
		}
	}
	return names, nil
}

type candidate struct {
	core, offset       int
	candidateSoloLimit bool
	session            string
	seq                int
	signal             machine.Signal
	// at is the seq a reset of the core must precede for the candidate to count.
	at int
}

// candidates lists, in seq order, the candidate solo limits and failure points the source's events yield, dropping those a later reset of their
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
			if in := intents[p.Trial]; p.Outcome == journal.OutcomeFailure && (in == nil || !in.RecordOnly) {
				signals[e.Seq] = p.Signal
			}
		case *journal.Shutdown:
			lastShutdown = e.Seq
		case *journal.HuntStart:
			hunts[p.Hunt] = p
		case *journal.Failure:
			if in := intents[p.Trial]; in == nil || !in.RecordOnly {
				signals[e.Seq] = p.Signal
			}
		case *journal.TrialCarried:
			if p.Outcome == journal.OutcomeFailure && !p.RecordOnly {
				signals[e.Seq] = p.Signal
			}
		case *journal.FailureCarried:
			signals[e.Seq] = p.Signal
		}
	}
	var all []candidate
	var inFlight *journal.Event
	for i, e := range s.events {
		switch p := e.Data.(type) {
		case *journal.TrialEnd:
			in := intents[p.Trial]
			if p.Outcome == journal.OutcomePass && in != nil && !in.RecordOnly && in.Condition == machine.Alone && in.Core != nil && in.Offset != nil {
				all = append(all, candidate{core: *in.Core, offset: *in.Offset, candidateSoloLimit: true, session: s.Session, seq: e.Seq, at: e.Seq})
			}
		case *journal.Failure:
			if in := intents[p.Trial]; in != nil && in.RecordOnly {
				continue
			}
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
				if cc.FailurePoint != nil {
					all = append(all, candidate{core: cc.Core, offset: *cc.FailurePoint, session: cc.FailurePointSession, seq: cc.FailurePointSeq, signal: cc.FailurePointSignal, at: e.Seq})
				}
				if cc.CandidateSoloLimit != nil {
					all = append(all, candidate{core: cc.Core, offset: *cc.CandidateSoloLimit, candidateSoloLimit: true, session: cc.CandidateSoloLimitSession, seq: cc.CandidateSoloLimitSeq, at: e.Seq})
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
		if !p.RecordOnly && p.Condition == machine.Alone && p.Core != nil && p.Offset != nil && *p.Offset != 0 {
			all = append(all, candidate{core: *p.Core, offset: *p.Offset, session: s.Session, seq: inFlight.Seq, signal: machine.Crash, at: inFlight.Seq})
		}
	}
	return slices.DeleteFunc(all, func(v candidate) bool { return v.at <= resetAt[v.core] })
}
