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
}

// auditEvents deliberately does not use tuner predicates or projected failure marks.
func auditEvents(events []journal.Event, closed bool) []violation {
	a := auditor{seen: make(map[int]journal.Event), points: make(map[int]point), combinations: make(map[int]combination), registers: make(map[int]int), pending: make(map[int]*pendingWrite), usedIntents: make(map[int]bool), reset: make(map[int]bool), trials: make(map[string]*journal.TrialIntent), hunts: make(map[int][]int)}
	for _, e := range events {
		a.fold(e)
	}
	// A live real journal may be read between a write and its readback.
	if closed {
		a.flushWrites()
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
		a.flushWrites()
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
	a.foldWrites(e)
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
	case *journal.StateRebuilt:
		a.add(e, "replay", "state.rebuilt records disagreement between state.json and journal projection")
	}
}
