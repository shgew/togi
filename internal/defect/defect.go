// Package defect identifies decisions made under known, since-fixed bugs.
package defect

import (
	"fmt"
	"slices"
	"time"

	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
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
	ID: 1, Title: "False failure at power-off", PR: 16, Direction: TooCautious,
	Detail:    "At power-off, systemd stopped the trial scope before shycler received SIGTERM; the backend exited cleanly (<nil>) but was reported as a failure and caused a proven backoff.",
	Decisions: []DecisionMatch{{Kind: journal.KindTunerDecision, Decision: journal.Backoff, Cause: journal.KindFailure, Predicate: powerOffFailure}},
}}

// Entries returns the defects known to this build, in increasing ID order.
func Entries() []Entry { return slices.Clone(entries) }

// Fixed is the highest defect ID fixed by this build.
func Fixed() int { return entries[len(entries)-1].ID }

// Find identifies each defect once per session, collecting all affected decisions and cores.
func Find(events []journal.Event) []Finding { return FindWith(events, entries) }

// FindWith uses a supplied list for scenarios where an entry is not yet in the binary.
func FindWith(events []journal.Event, list []Entry) []Finding {
	bySeq := make(map[int]journal.Event, len(events))
	ev := Evidence{Events: events, trialEnds: make(map[trialKey]journal.Event), trialProgress: make(map[trialKey][]journal.Event), stops: make(map[string][]journal.Event)}
	for _, e := range events {
		bySeq[e.Seq] = e
		switch p := e.Data.(type) {
		case *journal.TrialEnd:
			if p.Outcome == journal.OutcomeFailure && p.Signal == machine.UnexpectedExit {
				ev.trialEnds[trialKey{e.Boot, p.Trial}] = e
			}
		case *journal.TrialProgress:
			ev.trialProgress[trialKey{e.Boot, p.Trial}] = append(ev.trialProgress[trialKey{e.Boot, p.Trial}], e)
		case *journal.Shutdown:
			if p.Reason == journal.ShutdownSignal {
				ev.stops[e.Boot] = append(ev.stops[e.Boot], e)
			}
		}
	}
	findings := make([]Finding, 0, len(list))
	for _, entry := range list {
		finding := Finding{Entry: entry}
		fixed, found := 0, false
		for _, e := range events {
			switch p := e.Data.(type) {
			case *journal.SessionStart:
				fixed = p.Fixes
			case *journal.ConfigLoaded:
				fixed = p.Fixes
			case *journal.DefectFound:
				if p.ID == entry.ID {
					found = true
				}
			case *journal.TunerDecision:
				if fixed >= entry.ID || found {
					continue
				}
				for _, match := range entry.Decisions {
					if e.Kind != match.Kind || p.Decision != match.Decision {
						continue
					}
					for _, seq := range e.Cause {
						cause, ok := bySeq[seq]
						if ok && cause.Kind == match.Cause && (match.Predicate == nil || match.Predicate(ev, e, cause)) {
							finding.Decisions = append(finding.Decisions, e.Seq)
							if !slices.Contains(finding.Cores, p.Core) {
								finding.Cores = append(finding.Cores, p.Core)
							}
							break
						}
					}
					if slices.Contains(finding.Decisions, e.Seq) {
						break
					}
				}
			}
		}
		if !found && len(finding.Decisions) > 0 {
			slices.Sort(finding.Cores)
			findings = append(findings, finding)
		}
	}
	return findings
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
func Unanswered(events []journal.Event) []journal.DefectFound {
	answered := map[int]bool{}
	for _, e := range events {
		if p, ok := e.Data.(*journal.DefectAnswered); ok {
			answered[p.ID] = true
		}
	}
	var findings []journal.DefectFound
	for _, e := range events {
		if p, ok := e.Data.(*journal.DefectFound); ok && !answered[p.ID] {
			findings = append(findings, *p)
		}
	}
	return findings
}
