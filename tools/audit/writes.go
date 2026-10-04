package main

import (
	"fmt"
	"slices"

	"github.com/shgew/togi/internal/journal"
)

func (a *auditor) foldWrites(e journal.Event) {
	switch p := e.Data.(type) {
	case *journal.SessionBaseline:
		a.baseline = slices.Clone(p.Offsets)
		a.baselineRegisters()
	case *journal.SMUWrite:
		a.flushWrites()
		a.write(e, p)
	case *journal.SMUReadback:
		a.registers[p.Core] = p.Offset
		for _, cause := range e.Cause {
			if w := a.pending[cause]; w != nil {
				delete(w.cores, p.Core)
				if len(w.cores) == 0 {
					delete(a.pending, cause)
				}
			}
		}
	case *journal.ProfileApplied:
		a.flushWrites()
		a.appliedProfile(e, p.Offsets)
	case *journal.ProfileRestored:
		a.flushWrites()
		a.appliedProfile(e, p.Offsets)
	case *journal.TrialStart, *journal.TrialEnd, *journal.DeadEnd, *journal.Shutdown, *journal.SessionArchived:
		a.flushWrites()
	}
}
func (a *auditor) matchingIntent(e journal.Event, p *journal.SMUWrite) bool {
	for _, cause := range e.Cause {
		earlier, ok := a.seen[cause]
		if !ok || earlier.Boot != e.Boot || a.usedIntents[cause] {
			continue
		}
		intent, ok := earlier.Data.(*journal.SMUIntent)
		if ok && intent.Op == p.Op && intent.Offset == p.Offset && sameCore(intent.Core, p.Core) {
			a.usedIntents[cause] = true
			return true
		}
	}
	return false
}
func (a *auditor) write(e journal.Event, p *journal.SMUWrite) {
	if !a.matchingIntent(e, p) {
		a.add(e, "write_protocol", "SMU write has no matching earlier intent in this boot")
	}
	a.checkOffsets(e, []int{p.Offset})
	written := a.cores
	if p.Core != nil {
		written = []int{*p.Core}
	}
	waiting := make(map[int]bool)
	for _, core := range written {
		a.registers[core] = p.Offset
		waiting[core] = true
	}
	a.checkProfile(e, a.registers, written)
	a.pending[e.Seq] = &pendingWrite{e, waiting}
}
func (a *auditor) flushWrites() {
	seqs := make([]int, 0, len(a.pending))
	for seq := range a.pending {
		seqs = append(seqs, seq)
	}
	slices.Sort(seqs)
	for _, seq := range seqs {
		w := a.pending[seq]
		missing := make([]int, 0, len(w.cores))
		for core := range w.cores {
			missing = append(missing, core)
		}
		slices.Sort(missing)
		if len(missing) > 0 {
			a.add(w.event, "write_protocol", fmt.Sprintf("SMU write has no following readback for cores %v", missing))
		}
	}
	clear(a.pending)
}
func sameCore(a, b *int) bool { return a == nil && b == nil || a != nil && b != nil && *a == *b }
