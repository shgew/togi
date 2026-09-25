package tuner

import (
	"fmt"
	"slices"

	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
)

const (
	queuedRegain = "regain"
	queuedReset  = "reset"
)

func (s *State) queueRegain(seq int, cores []int) {
	for _, id := range cores {
		if c := s.core(id); c != nil && c.regainable() {
			c.queued, c.queueSeq = queuedRegain, seq
		}
	}
}

func (c *core) regainable() bool {
	return c.phase == journal.PhaseConfirmed && c.unproven > 0 && c.queued == ""
}

// queued starts the first core in scheduling order whose queued command is kind.
func (s *State) queued(kind string) (Action, bool) {
	for _, c := range s.cores {
		if c.queued != kind {
			continue
		}
		var p journal.Payload
		switch kind {
		case queuedReset:
			p = reset(c.snapshot(), c.baseline)
		case queuedRegain:
			p = regainStart(c.snapshot())
		}
		return Action{Kind: Decide, Payload: p, Cause: []int{c.queueSeq}}, true
	}
	return Action{}, false
}

// Regainable lists, in scheduling order, the confirmed cores with unproven depth and nothing queued.
func (s *State) Regainable() []int {
	var out []int
	for _, c := range s.cores {
		if c.regainable() {
			out = append(out, c.id)
		}
	}
	return out
}

// RegainPending reports a regain that is queued or running.
func (s *State) RegainPending() bool {
	return slices.ContainsFunc(s.cores, func(c *core) bool { return c.queued == queuedRegain || c.phase == journal.PhaseRegain })
}

func reset(c coreState, baseline int) *journal.CorePhase {
	return &journal.CorePhase{
		Core: c.core, From: c.phase, To: journal.PhaseSearch, Offset: machine.ClampOffset(baseline),
		Reason: fmt.Sprintf("reset: failed mark, unproven depth and confirmation cleared; search restarts from the baseline %d", baseline),
	}
}

func regainStart(c coreState) *journal.CorePhase {
	return &journal.CorePhase{
		Core: c.core, From: journal.PhaseConfirmed, To: journal.PhaseRegain, Offset: c.offset - 1,
		Pass: c.pass, FailedMark: c.fail, UnprovenDepth: c.unproven,
		Reason: fmt.Sprintf("regain: one count deeper, undoing one of %d suspect counts", c.unproven),
	}
}

func regained(c coreState) *journal.CorePhase {
	return &journal.CorePhase{
		Core: c.core, From: journal.PhaseRegain, To: journal.PhaseConfirmed, Offset: c.offset,
		Pass: c.pass, FailedMark: c.fail, UnprovenDepth: c.unproven - 1, Reason: "R1 to R5 passed; one count regained",
	}
}

func regainFailure(c coreState) *journal.CorePhase {
	o := c.offset
	pass, discarded := keepPass(c.pass, o)
	return &journal.CorePhase{
		Core: c.core, From: journal.PhaseRegain, To: journal.PhaseConfirmed, Offset: o + 1, Pass: pass, FailedMark: new(o), Backoff: true,
		Reason: fmt.Sprintf("failed regain at %d: proven backoff to %d, confirmed there before; the failed mark cancels the remaining unproven depth%s", o, o+1, discarded),
	}
}
