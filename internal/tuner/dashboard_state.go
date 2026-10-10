package tuner

import (
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"slices"
)

// CoreLimit returns what prevents one more count of depth at the current profile.
func (s *State) CoreLimit(id int) Limit {
	c := s.core(id)
	if c == nil {
		return Limit{}
	}
	l, _, _ := s.limitAt(c, s.offsets())
	return l
}

func (s *State) inFlight() *journal.TrialIntent { return s.flight }

func (s *State) partCCD(ids []int) int {
	if len(ids) == 0 {
		return -1
	}
	ccd := s.ccd[ids[0]]
	for _, id := range ids {
		if s.ccd[id] != ccd {
			return -1
		}
	}
	return ccd
}

func (s *State) cyclePart(req []requirement, since int, rule evidenceRule) CyclePart {
	p := CyclePart{Cores: slices.Clone(req[0].cores), CCD: s.partCCD(req[0].cores), Done: true}
	fullSize := len(s.cores)
	if p.CCD >= 0 {
		fullSize = 0
		for _, c := range s.cores {
			if s.ccd[c.id] == p.CCD {
				fullSize++
			}
		}
	}
	p.Full = len(p.Cores) == fullSize
	p.Partial = req[0].class.regime == machine.R7 && !p.Full
	for _, q := range req {
		if q.count == 0 {
			continue
		}
		if p.ShortS == 0 {
			p.ShortS = q.class.duration
			p.Short = q.count
		} else {
			p.LongS = q.class.duration
			p.Long = q.count
		}
		passed := s.passes(q.class, s.checking.profile, since, rule)
		p.Passed += passed
		p.Failed += s.cycleFailures(q.class, s.checking.profile, since)
		p.Done = p.Done && passed >= q.count
	}
	return p
}

func (s *State) cycleFailures(k trialClass, profile []int, since int) int {
	failed := 0
	for _, e := range s.ledger[k] {
		if e.seq > since && !e.pass && !e.carried && AtLeastDeep(e.profile, profile) {
			failed++
		}
	}
	return failed
}

// CyclePlan projects requirements using the same scheduling classes as Next.
func (s *State) CyclePlan() CyclePlan {
	g := &s.checking
	out := CyclePlan{Number: g.cycle, Open: g.open, Current: len(g.steps), Paused: s.hunt != nil || len(s.obligations) > 0}
	running := s.inFlight()
	if running != nil && (running.Cycle != g.cycle || running.Hunt > 0 || running.Round > 0 || running.Rerun) {
		running = nil
	}
	for i, r := range g.steps {
		req := s.requirements(i)
		step := CycleStep{Regime: r, ChainsComplete: r != machine.R7 || s.r7ChainsComplete(i)}
		step.Done = step.ChainsComplete
		if len(req) > 0 {
			for _, w := range machine.Workloads(r) {
				if w.ID == req[0].class.workload {
					step.Workload = w
					break
				}
			}
		}
		for k := 0; k < len(req); {
			j := k + 1
			for j < len(req) && slices.Equal(req[k].cores, req[j].cores) {
				j++
			}
			partReq, since, rule := req[k:j], g.startSeq, cycleEvidence
			isRunning := false
			if running != nil {
				stored := s.scheduled[running.Trial]
				q := stored.requirement
				isRunning = q.Kind == "cycle" && q.Step == i+1 && q.Part == len(step.Parts)+1
				if isRunning {
					partReq, since, rule = stored.part, q.Since, q.Rule
				}
			}
			part := s.cyclePart(partReq, since, rule)
			part.Running = isRunning
			step.Parts = append(step.Parts, part)
			k = j
		}
		for _, p := range step.Parts {
			step.Done = step.Done && p.Done
		}
		if !g.open && g.stepsDone == len(g.steps) {
			step.Done = true
			for j := range step.Parts {
				step.Parts[j].Done = true
			}
		}
		if !step.Done && out.Current == len(g.steps) {
			out.Current = i
		}
		out.Steps = append(out.Steps, step)
	}
	return out
}

// SearchTurns returns the tuner's rotating core order, with unfinished two-regime steps first.
func (s *State) SearchTurns() []SearchTurn {
	var ids []int
	if s.cursor >= 0 {
		c := s.cores[s.cursor]
		if c.phase == journal.PhaseSearch && !c.check && c.stepR1 && len(c.stepSeqs) == 1 {
			ids = append(ids, c.id)
		}
	}
	for i := range s.cores {
		c := s.cores[(s.cursor+1+i)%len(s.cores)]
		if c.phase == journal.PhaseSearch && !slices.Contains(ids, c.id) {
			ids = append(ids, c.id)
		}
	}
	var out []SearchTurn
	running := s.inFlight()
	for _, id := range ids {
		c := s.core(id)
		t := SearchTurn{Core: id, Confirm: c.check, Offset: c.offset, Step: 5, Needed: s.n, Regimes: []machine.Regime{machine.R1, machine.R2}, Running: running != nil && running.Core != nil && *running.Core == id && running.Phase == journal.PhaseSearch}
		if c.fail != nil {
			t.Step = 1
		}
		r := machine.R1
		w := ""
		if !c.check && c.stepR1 && len(c.stepSeqs) == 1 {
			t.Regimes = []machine.Regime{machine.R2}
			r = machine.R2
		}
		if c.check {
			p := make([]int, len(s.cores))
			p[s.index(id)] = c.offset
			for j, regime := range []machine.Regime{machine.R1, machine.R2} {
				count := s.passes(trialClass{regime, c.checkWorkloads[j], coresKey([]int{id}), s.durations.SearchTrialS}, p, c.phaseSeq, soloLimitEvidence)
				if j == 0 {
					t.Light = count
				} else {
					t.Heavy = count
				}
			}
			if t.Light >= s.n {
				r = machine.R2
				w = c.checkWorkloads[1]
			} else {
				w = c.checkWorkloads[0]
			}
			t.Step = 0
		}
		_, t.Workload, _ = s.searchTrial(c, r, w).Trial.Complete(c.workloadIndex[r], s.offsets())
		out = append(out, t)
	}
	return out
}

// PhasePlan reports where the two-phase method stands and, in phase 2, the worst-case remaining work: the rounds that
// would still run if none failed, then one full cycle. It is nil before the session has cores.
func (s *State) PhasePlan() *journal.PhasesState {
	if len(s.cores) == 0 {
		return nil
	}
	ph := &s.phases
	out := &journal.PhasesState{Phase: 2, Phase1End: ph.phase1End, Concluded: ph.concluded}
	switch {
	case ph.phase1End == 0:
		out.Phase = 1
		return out
	case ph.concluded != 0:
		out.Phase = 0
		return out
	case ph.confirmStart != 0:
		out.Confirming = true
		return out
	}
	gaps := map[int]int{}
	carried := map[int]bool{}
	moving := map[int]bool{}
	from, passed := s.offsets(), ph.passed
	if r := s.round; r != nil {
		out.RoundsLeft, out.Round = 1, r.start.Round
		for _, id := range r.start.Cores {
			if m, ok := ph.moves[id]; ok && m.round == r.start.Round {
				gaps[id]++
				moving[id] = true
			} else {
				carried[id] = true
			}
		}
		from, passed = slices.Clone(r.start.Profile), slices.Clone(r.start.Profile)
	}
	for range 2 - machine.MinOffset {
		profile, moved, kept := s.phase2MovesAt(from, passed)
		if len(moved)+len(kept) == 0 {
			break
		}
		out.RoundsLeft++
		for _, id := range moved {
			gaps[id]++
		}
		for _, id := range kept {
			carried[id] = true
		}
		from, passed = profile, profile
	}
	out.Confirming = out.RoundsLeft == 0
	for _, c := range s.byID() {
		if gaps[c.id] == 0 && !carried[c.id] {
			continue
		}
		offset := c.offset
		if moving[c.id] {
			offset = s.round.initial[s.index(c.id)]
		}
		out.Candidates = append(out.Candidates, journal.CandidateState{Core: c.id, Offset: offset, SoloLimit: c.soloLimit, Gap: gaps[c.id], Carried: carried[c.id], Moving: moving[c.id]})
	}
	return out
}

// Requirement returns the evidence count used to schedule this trial; Trial is one-based.
func (s *State) Requirement(p *journal.TrialIntent) TrialRequirement {
	if p == nil {
		return TrialRequirement{}
	}
	q, ok := s.scheduled[p.Trial]
	if !ok {
		q = s.scheduledFor(p, nil)
	}
	if q.ended != nil {
		return q.ended.Requirement
	}
	return s.requirementProgress(p, q.requirement)
}

func (s *State) rerunPlan(k trialClass) RerunPlan {
	p := RerunPlan{Regime: k.regime, Cores: slices.Clone(s.classTargets[k.cores].cores), Short: s.n, ShortS: s.durations.ShortTrialS}
	if k.duration != p.ShortS {
		p.Long = 1
		p.LongS = k.duration
	}
	return p
}
