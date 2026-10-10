package main

import (
	"fmt"
	"slices"

	"github.com/shgew/togi/internal/journal"
)

type violation struct {
	Directory string `json:"directory"`
	Journal   string `json:"journal"`
	Session   string `json:"session"`
	Seq       int    `json:"seq"`
	Check     string `json:"check"`
	Reason    string `json:"reason"`
	Scenario  string `json:"scenario,omitempty"`
	Seed      uint64 `json:"seed,omitempty"`
}
type point struct{ offset, seq int }
type combination struct {
	members []journal.CombinationMember
	seq     int
}
type pendingWrite struct {
	event journal.Event
	cores map[int]bool
}
type auditor struct {
	found           []violation
	session, boot   string
	cores, baseline []int
	lastStop        bool
	seen            map[int]journal.Event
	points          map[int]point
	combinations    map[int]combination
	registers       map[int]int
	pending         map[int]*pendingWrite
	usedIntents     map[int]bool
	reset           map[int]bool
	trials          map[string]*journal.TrialIntent
	hunts           map[int][]int
	zeroNamed       map[int]int
	// build is the build of the latest config.loaded, or of session.start before the first.
	build journal.Build
	// rebuilt is a state.rebuilt without last_seq awaiting its run's config.loaded.
	rebuilt *journal.Event
}

// auditEvents deliberately does not use tuner predicates or projected failure marks.
func auditEvents(events []journal.Event, closed bool) []violation {
	a := auditor{seen: make(map[int]journal.Event), points: make(map[int]point), combinations: make(map[int]combination), registers: make(map[int]int), pending: make(map[int]*pendingWrite), usedIntents: make(map[int]bool), reset: make(map[int]bool), trials: make(map[string]*journal.TrialIntent), hunts: make(map[int][]int), zeroNamed: make(map[int]int)}
	for _, e := range events {
		a.fold(e)
	}
	// A live real journal may be read between a write and its readback.
	if closed {
		a.flushWrites()
		a.flushRebuild()
		if len(events) > 0 && !a.lastStop {
			a.add(events[len(events)-1], "termination", "finished simulated run has neither conclusion nor dead end with a reason")
		}
	}
	return a.found
}
func (a *auditor) add(e journal.Event, check, reason string) {
	a.found = append(a.found, violation{Session: a.session, Seq: e.Seq, Check: check, Reason: reason})
}
func (a *auditor) fold(e journal.Event) {
	if p, ok := e.Data.(*journal.SessionStart); ok {
		a.session = p.Session
		a.cores = nil
		for _, c := range p.Cores {
			a.cores = append(a.cores, c.Core)
		}
		slices.Sort(a.cores)
	}
	if e.Boot != a.boot {
		// The rebuild's run records config.loaded in its own boot.
		a.flushRebuild()
		// journal.md rule 2: a reboot may interrupt a write before its readback.
		clear(a.pending)
		a.boot = e.Boot
		clear(a.registers)
		a.baselineRegisters()
	}
	for _, cause := range e.Cause {
		if _, ok := a.seen[cause]; !ok || cause >= e.Seq {
			a.add(e, "cause", fmt.Sprintf("%s cause %d is not an existing earlier sequence", e.Kind, cause))
		}
	}
	a.foldMarks(e)
	a.checkChosenOffsets(e)
	a.foldWrites(e)
	a.foldBuild(e)
	a.foldConclusion(e)
	a.seen[e.Seq] = e
}
func (a *auditor) baselineRegisters() {
	for i, core := range a.cores {
		if i < len(a.baseline) {
			a.registers[core] = a.baseline[i]
		}
	}
}
func (a *auditor) foldConclusion(e journal.Event) {
	previous := a.lastStop
	a.lastStop = false
	switch p := e.Data.(type) {
	case *journal.DeadEnd:
		if p.Condition == "" || p.Detail == "" {
			a.add(e, "termination", "dead end has no condition or reason")
		}
		a.lastStop = true
	case *journal.Shutdown:
		if p.Reason == "" {
			a.add(e, "termination", "shutdown has no reason")
		}
		a.lastStop = true
	case *journal.SessionArchived:
		a.lastStop = true
	case *journal.CommandReset:
		a.lastStop = p.All
	case *journal.SessionWarning:
		// Nonfatal projection warnings may follow the final shutdown.
		a.lastStop = previous
	}
}

// foldBuild judges a state.rebuilt that omits last_seq: a current snapshot disagreed with replay. A run checks its
// snapshot before it records config.loaded, so that run's config.loaded names the build that replayed. A build other
// than the previous run's may project fields an older snapshot lacks, which is a legitimate upgrade; the same build
// disagreeing with its own current snapshot is a violation.
func (a *auditor) foldBuild(e journal.Event) {
	switch p := e.Data.(type) {
	case *journal.SessionStart:
		a.build = p.Build
	case *journal.ConfigLoaded:
		if a.rebuilt != nil && a.build == p.Build {
			a.add(*a.rebuilt, "replay", "state.rebuilt records disagreement with a current state.json snapshot")
		}
		a.rebuilt = nil
		a.build = p.Build
	case *journal.StateRebuilt:
		if !slices.Contains(p.Fields, "last_seq") {
			a.flushRebuild()
			a.rebuilt = &e
		}
	case *journal.Shutdown, *journal.DeadEnd, *journal.SessionArchived:
		a.flushRebuild()
	}
}

// flushRebuild flags a rebuild whose run ended without config.loaded, so no build change explains it. A run ends at
// its shutdown, a dead end, a session archive or a later boot; a finished simulated journal ends its last run. A live
// real journal may be read before its run records config.loaded, so a rebuild pending at its end is deferred.
func (a *auditor) flushRebuild() {
	if a.rebuilt != nil {
		a.add(*a.rebuilt, "replay", "state.rebuilt records disagreement with a current state.json snapshot")
		a.rebuilt = nil
	}
}
