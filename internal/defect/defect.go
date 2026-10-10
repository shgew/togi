// Package defect identifies decisions made under known, since-fixed bugs.
package defect

import (
	"fmt"
	"slices"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type Direction string

const (
	TooCautious   Direction = "too_cautious"
	TooAggressive Direction = "too_aggressive"
)

type DecisionMatch struct {
	Kind      journal.Kind
	Decision  journal.Decision
	Cause     journal.Kind
	Predicate func(Evidence, journal.Event, journal.Event) bool
}

type trialKey struct{ boot, trial string }

// Evidence supplies the journal and indexed facts to a defect predicate.
type Evidence struct {
	Events        []journal.Event
	trialEnds     map[trialKey]journal.Event
	trialProgress map[trialKey][]journal.Event
	stops         map[string][]journal.Event
}

type Entry struct {
	ID        int
	Title     string
	Detail    string
	PR        int
	Direction Direction
	Decisions []DecisionMatch
}

type Finding struct {
	Entry     Entry
	Cores     []int
	Decisions []int
}

var entries = []Entry{{
	ID: 1, Title: "False failure at power-off", PR: 14, Direction: TooCautious,
	Detail:    "At power-off, systemd stopped the trial scope before togi received SIGTERM; the backend exited cleanly (<nil>) but was reported as a failure and caused a proven backoff.",
	Decisions: []DecisionMatch{{Kind: journal.KindTunerDecision, Decision: journal.Backoff, Cause: journal.KindFailure, Predicate: powerOffFailure}},
}}

// Entries returns the defects known to this build, in increasing ID order.
func Entries() []Entry { return slices.Clone(entries) }

// Fixed is the highest defect ID fixed by this build.
func Fixed() int { return entries[len(entries)-1].ID }

// FindWith uses a supplied list for scenarios where an entry is not yet in the binary.
func FindWith(events []journal.Event, list []Entry) []Finding {
	return new(Index).Find(events, list)
}

// FailuresWith returns, ascending, the seqs of failure events behind decisions a listed defect matches, whether or not a
// finding was recorded.
func FailuresWith(events []journal.Event, list []Entry) []int {
	return new(Index).Failures(events, list)
}

// Unanswered returns recorded findings that have not yet received an answer.
func Unanswered(events []journal.Event) []journal.DefectFound {
	return new(Index).Unanswered(events)
}

type decisionAt struct {
	pos int
	// fixed is the highest defect ID the build that made the decision had fixed.
	fixed int
}

// Index is what the defect queries read from a journal, built once and extended by the events appended since: each
// query first folds events it has not seen. It follows one append-only journal; given a shorter one, it starts over.
// The zero value is an empty index.
type Index struct {
	scanned   int
	fixed     int
	position  map[int]int
	ev        Evidence
	decisions []decisionAt
	recorded  map[int]bool
	answered  map[int]bool
	yes       []int
	found     []int
}

func (x *Index) sync(events []journal.Event) {
	if len(events) < x.scanned {
		*x = Index{}
	}
	if x.position == nil {
		x.position = make(map[int]int)
		x.ev = Evidence{trialEnds: make(map[trialKey]journal.Event), trialProgress: make(map[trialKey][]journal.Event), stops: make(map[string][]journal.Event)}
		x.recorded = make(map[int]bool)
		x.answered = make(map[int]bool)
	}
	x.ev.Events = events
	for pos := x.scanned; pos < len(events); pos++ {
		e := events[pos]
		x.position[e.Seq] = pos
		switch p := e.Data.(type) {
		case *journal.SessionStart:
			x.fixed = p.Fixes
		case *journal.ConfigLoaded:
			x.fixed = p.Fixes
		case *journal.TunerDecision:
			x.decisions = append(x.decisions, decisionAt{pos: pos, fixed: x.fixed})
		case *journal.TrialEnd:
			if p.Outcome == journal.OutcomeFailure && p.Signal == machine.UnexpectedExit {
				x.ev.trialEnds[trialKey{e.Boot, p.Trial}] = e
			}
		case *journal.TrialProgress:
			x.ev.trialProgress[trialKey{e.Boot, p.Trial}] = append(x.ev.trialProgress[trialKey{e.Boot, p.Trial}], e)
		case *journal.Shutdown:
			if p.Reason == journal.ShutdownSignal {
				x.ev.stops[e.Boot] = append(x.ev.stops[e.Boot], e)
			}
		case *journal.DefectFound:
			x.recorded[p.ID] = true
			x.found = append(x.found, pos)
		case *journal.DefectAnswered:
			x.answered[p.ID] = true
			if p.Answer == "yes" {
				x.yes = append(x.yes, pos)
			}
		}
	}
	x.scanned = len(events)
}

// Find returns the listed defects some decision in events was made under and a finding was not yet recorded for.
func (x *Index) Find(events []journal.Event, list []Entry) []Finding {
	findings := make([]Finding, len(list))
	x.scan(events, list, true, func(i int, decision, _ journal.Event) {
		core := decision.Data.(*journal.TunerDecision).Core
		findings[i].Decisions = append(findings[i].Decisions, decision.Seq)
		if !slices.Contains(findings[i].Cores, core) {
			findings[i].Cores = append(findings[i].Cores, core)
		}
	})
	found := make([]Finding, 0, len(list))
	for i, f := range findings {
		if len(f.Decisions) > 0 {
			f.Entry = list[i]
			slices.Sort(f.Cores)
			found = append(found, f)
		}
	}
	return found
}

// Failures is FailuresWith over events.
func (x *Index) Failures(events []journal.Event, list []Entry) []int {
	var seqs []int
	x.scan(events, list, false, func(_ int, _, cause journal.Event) {
		if cause.Kind == journal.KindFailure && !slices.Contains(seqs, cause.Seq) {
			seqs = append(seqs, cause.Seq)
		}
	})
	slices.Sort(seqs)
	return seqs
}

// scan visits each decision a listed defect matches, with the cause it matched, skipping decisions made by a build that
// fixed the defect and, when skipFound is set, every defect a finding was recorded for.
func (x *Index) scan(events []journal.Event, list []Entry, skipFound bool, visit func(i int, decision, cause journal.Event)) {
	x.sync(events)
	for i, entry := range list {
		if skipFound && x.recorded[entry.ID] {
			continue
		}
		for _, d := range x.decisions {
			if d.fixed >= entry.ID {
				continue
			}
			e := events[d.pos]
			visitMatch(x.ev, events, x.position, entry, e, e.Data.(*journal.TunerDecision).Decision, func(cause journal.Event) { visit(i, e, cause) })
		}
	}
}

func visitMatch(ev Evidence, events []journal.Event, position map[int]int, entry Entry, e journal.Event, decision journal.Decision, visit func(cause journal.Event)) {
	for _, match := range entry.Decisions {
		if e.Kind != match.Kind || decision != match.Decision {
			continue
		}
		for _, seq := range e.Cause {
			pos, ok := position[seq]
			if !ok {
				continue
			}
			cause := events[pos]
			if cause.Kind == match.Cause && (match.Predicate == nil || match.Predicate(ev, e, cause)) {
				visit(cause)
				return
			}
		}
	}
}

func powerOffFailure(ev Evidence, decision, cause journal.Event) bool {
	failure := cause.Data.(*journal.Failure)
	if failure.Signal != machine.UnexpectedExit || failure.Trial == "" {
		return false
	}
	end, ok := ev.trialEnds[trialKey{cause.Boot, failure.Trial}]
	if !ok || end.Seq >= cause.Seq {
		return false
	}
	cleanExit := fmt.Sprintf("core %02d backend exited early: <nil>", decision.Data.(*journal.TunerDecision).Core)
	matched := false
	for _, progress := range ev.trialProgress[trialKey{cause.Boot, failure.Trial}] {
		if progress.Seq < end.Seq && progress.Data.(*journal.TrialProgress).Detail == cleanExit {
			matched = true
			break
		}
	}
	if !matched {
		return false
	}
	for _, stop := range ev.stops[end.Boot] {
		if stop.Seq > decision.Seq && !stop.Time.Before(end.Time) && stop.Time.Sub(end.Time) <= 5*time.Second {
			return true
		}
	}
	return false
}

// Unanswered returns recorded findings that have not yet received an answer.
func (x *Index) Unanswered(events []journal.Event) []journal.DefectFound {
	x.sync(events)
	var findings []journal.DefectFound
	for _, pos := range x.found {
		if p := events[pos].Data.(*journal.DefectFound); !x.answered[p.ID] {
			findings = append(findings, *p)
		}
	}
	return findings
}

// FoundSeq returns the seq of the first finding recorded for the defect, or 0.
func (x *Index) FoundSeq(events []journal.Event, id int) int {
	x.sync(events)
	for _, pos := range x.found {
		if events[pos].Data.(*journal.DefectFound).ID == id {
			return events[pos].Seq
		}
	}
	return 0
}

// Yes returns the events that answered yes to resetting a defect's cores, in order.
func (x *Index) Yes(events []journal.Event) []journal.Event {
	x.sync(events)
	out := make([]journal.Event, len(x.yes))
	for i, pos := range x.yes {
		out[i] = events[pos]
	}
	return out
}
