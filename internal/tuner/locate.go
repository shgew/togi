package tuner

import (
	"fmt"
	"slices"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

// locatedHunt records how a located hunt answered an unattributed multi-core R7 failure: open while result is
// empty, consumed by any end but loaded, and charged to the loaded cores after a loaded end. A loaded end whose
// group failure named a loaded core records that core: the named failure decides the charge. allZero marks a hunt
// whose loaded cores were all at CO 0, so its locate was the failure's all-zero rerun.
type locatedHunt struct {
	hunt, end, failure, source int
	named                      *int
	result                     string
	allZero                    bool
}

// located reports a hunt of a failed multi-core R7 load: it keeps the loaded cores at their failing offsets
// and narrows only the unloaded ones. Multi-core R7 hunts of earlier rulesets had loaded candidates.
func (h *hunt) located() bool {
	return h.start.Regime == machine.R7 && len(h.start.Cores) > 1 && !slices.ContainsFunc(h.start.Candidates, func(id int) bool { return slices.Contains(h.start.Cores, id) })
}

// narrowing returns the groups that narrow the candidates: every group but a located hunt's first, locate.
func (h *hunt) narrowing() []groupRecord {
	if h.located() && len(h.groups) > 0 {
		return h.groups[1:]
	}
	return h.groups
}

// locatedFull reports a located hunt's full group at the failed duration: the failing profile itself, which must
// pass before narrowing that found no failing group ends the hunt loaded. A short pass does not clear a long failure.
func (h *hunt) locatedFull(p groupPlan) bool {
	return h.located() && p.stage == "full" && p.duration == h.start.DurationS
}

// locatePlan decides a located hunt's plan until its locate group has passed: the locate group itself, or the
// end `loaded` once it failed or was skipped.
func (s *State) locatePlan(h *hunt) (plan groupPlan, has, decided bool) {
	if !h.located() {
		return groupPlan{}, false, false
	}
	plan = groupPlan{set: slices.Clone(h.start.Candidates), g: 2, stage: "locate", duration: h.start.DurationS}
	if len(h.groups) == 0 {
		return plan, true, true
	}
	outcome := s.groupOutcome(h, h.groups[0])
	if outcome == "pass" {
		return groupPlan{}, false, false
	}
	plan.result, plan.loaded = outcome != "running", true
	return plan, false, true
}

// locatable returns the failed trial of a multi-core R7 failure that is located before it is charged to the loaded
// cores: a live unattributed one whose load left an unloaded core nonzero, and, carried or naming a core at CO 0,
// one whose loaded cores were all at CO 0, where locate is its all-zero rerun.
func (s *State) locatable(f pendingFailure) *entry {
	if !s.multiR7(f.class) || !f.carried && f.failure.Condition == machine.Parked || len(s.locateCandidates(f)) == 0 {
		return nil
	}
	failed := s.r7FailureEntry(f)
	switch {
	case failed == nil:
		return nil
	case s.loadedAtZero(f):
		if failed.named != nil && !s.atCOZero(f.profile, *failed.named) {
			return nil
		}
		return failed
	case f.carried || f.failure.Condition != machine.Together || s.r7NamedCulprit(*failed):
		return nil
	}
	return failed
}

func (s *State) loadedAtZero(f pendingFailure) bool {
	return !slices.ContainsFunc(s.classCores(f.class), func(id int) bool { return !s.atCOZero(f.profile, id) })
}

func (s *State) atCOZero(profile []int, id int) bool {
	i := s.index(id)
	return i < 0 || i >= len(profile) || profile[i] == 0
}

func (s *State) locateCandidates(f pendingFailure) []int {
	loaded := s.classCores(f.class)
	var out []int
	for i, c := range s.byID() {
		if i < len(f.profile) && f.profile[i] != 0 && !slices.Contains(loaded, c.id) {
			out = append(out, c.id)
		}
	}
	return out
}

// locateDue returns the first failure that waits for its located hunt and would otherwise move a core or end
// the session.
func (s *State) locateDue() (pendingFailure, bool) {
	if s.hunt != nil {
		return pendingFailure{}, false
	}
	for _, f := range s.pendingFailures {
		if _, ok := s.located[f.seq]; ok {
			continue
		}
		failed := s.locatable(f)
		if failed == nil {
			continue
		}
		for _, id := range s.failureTargets(*failed) {
			if !s.r7Handled[f.seq][id] && !s.r7Answered(*failed, id) {
				return f, true
			}
		}
	}
	return pendingFailure{}, false
}

// locateNext starts the located hunt a failure waits for, once a deepening round has ended and the ranking is read.
func (s *State) locateNext() (Action, bool) {
	f, ok := s.locateDue()
	switch {
	case !ok:
		return Action{}, false
	case s.round != nil:
		return Action{Kind: Decide, Payload: &journal.DeepeningRound{Round: s.round.start.Round, Event: journal.CycleEnd, Reason: fmt.Sprintf("R7 failure #%d needs a located hunt", f.seq)}, Cause: []int{f.seq}}, true
	case s.rankingSeq <= s.lastPlanSeq:
		return Action{Kind: ReadRanking}, true
	}
	return s.locateStart(f), true
}

func (s *State) locateStart(f pendingFailure) Action {
	loaded := slices.Clone(s.classCores(f.class))
	parked := slices.Clone(f.profile)
	for i, c := range s.byID() {
		if !slices.Contains(loaded, c.id) {
			parked[i] = 0
		}
	}
	p := &journal.HuntStart{Hunt: s.nextHunt + 1, Failure: f.seq, Trial: f.failure.Trial, Regime: f.class.regime, Workload: f.class.workload, Cores: loaded, DurationS: f.class.duration, Failing: slices.Clone(f.profile), Parked: parked, Candidates: s.locateCandidates(f), Trials: s.n, TrialS: s.durations.ShortTrialS, Miss: s.evidence.Miss, Rate: s.evidence.Rate, Ranking: slices.Clone(s.ranking)}
	p.Reason = "no core is named, so the failure is located on the unloaded cores before it is charged to the loaded cores"
	return Action{Kind: Decide, Payload: p, Cause: []int{f.seq}}
}

// locatedCulprit returns the core a parked located-hunt failure names directly: an unloaded core at a nonzero
// offset. A loaded core named ends the hunt loaded instead, and an unloaded core named at CO 0 leaves the group
// failure unattributed.
func (s *State) locatedCulprit(f pendingFailure) *core {
	p := f.failure
	if f.carried || p.Condition != machine.Parked || p.Attribution != journal.Attributed || p.Core == nil || p.Offset == nil || *p.Offset == 0 || slices.Contains(s.classCores(f.class), *p.Core) {
		return nil
	}
	return s.core(*p.Core)
}

// locateFailure returns the failed trial that rejected the hunt's latest group, or 0.
func (s *State) locateFailure(h *hunt) int {
	if len(h.groups) == 0 {
		return 0
	}
	_, failure, _ := s.groupEvidence(h, h.groups[len(h.groups)-1], true)
	return failure
}

// allZeroLocated reports a located hunt whose loaded cores were all at CO 0: its locate is the all-zero rerun.
func (s *State) allZeroLocated(h *hunt) bool {
	return !slices.ContainsFunc(h.start.Cores, func(id int) bool { return !s.atCOZero(h.start.Failing, id) })
}

// locatedLoadedNamed returns the failure that rejected the hunt's latest group, or 0, and the loaded core it named,
// if any. After a passed all-zero locate, a loaded core named at CO 0 leaves the group failure unattributed.
func (s *State) locatedLoadedNamed(h *hunt) (int, *int) {
	failure := s.locateFailure(h)
	if failure == 0 || len(h.groups) > 1 && s.allZeroLocated(h) {
		return failure, nil
	}
	for _, e := range s.ledger[h.class.withDuration(h.groups[len(h.groups)-1].payload.DurationS)] {
		if e.seq == failure && e.named != nil && slices.Contains(h.start.Cores, *e.named) {
			return failure, new(*e.named)
		}
	}
	return failure, nil
}

// locatedLoadedFailure ends a located hunt loaded when its latest group failed naming a loaded core.
func (s *State) locatedLoadedFailure(h *hunt) (Action, bool) {
	if !h.located() {
		return Action{}, false
	}
	failure, named := s.locatedLoadedNamed(h)
	if named == nil {
		return Action{}, false
	}
	reason := fmt.Sprintf("failure #%d named loaded core %02d, so the failure stays with the loaded cores and core %02d is charged for it", failure, *named, *named)
	return Action{Kind: Decide, Payload: &journal.HuntEnd{Hunt: h.start.Hunt, Result: "loaded", Cores: slices.Clone(h.start.Cores), Groups: len(h.groups), Reason: reason}, Cause: []int{h.seq, failure}}, true
}

func (s *State) endLocatedHunt(h *hunt, e journal.Event, p *journal.HuntEnd) {
	switch p.Result {
	case "cancelled":
		delete(s.located, h.start.Failure)
		s.hunt = nil
	case "loaded":
		failure, named := s.locatedLoadedNamed(h)
		s.located[h.start.Failure] = locatedHunt{hunt: p.Hunt, end: e.Seq, failure: failure, source: h.start.Failure, named: named, result: p.Result, allZero: s.allZeroLocated(h)}
		s.hunt = nil
	default:
		s.located[h.start.Failure] = locatedHunt{hunt: p.Hunt, end: e.Seq, result: p.Result}
	}
}
