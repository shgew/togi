package tuner

import (
	"fmt"
	"slices"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/requests"
)

func (s *State) ccdParts() [][]int {
	parts := s.parts
	if len(parts) > 1 {
		parts = parts[:len(parts)-1]
	}
	return parts
}

func (s *State) startCheckingStep(step int) *journal.CheckingStep {
	g := &s.checking
	return &journal.CheckingStep{Cycle: g.cycle, Step: step + 1, Profile: slices.Clone(g.profile)}
}

func (s *State) recordCheckingStep(e journal.Event, p *journal.CheckingStep) {
	g := &s.checking
	if p.Cycle != g.cycle || !g.open {
		return
	}
	if g.partial == nil {
		g.partial = map[int]*checkingStep{}
	}
	g.partial[p.Step] = &checkingStep{start: p, seq: e.Seq, chains: map[int][]*checkingChain{}, completed: map[trialClass]int{}, passed: map[trialClass]int{}, failed: map[trialClass]int{}}
	g.lastSeq = e.Seq
	s.projectionDirty = true
}

func (s *State) recordCheckingChain(e journal.Event, p *journal.CheckingChain) {
	g := &s.checking
	step := g.partial[p.Step]
	if !g.open || p.Cycle != g.cycle || step == nil {
		return
	}
	chain := step.chains[p.CCD]
	for i, node := range chain {
		if node.start.Part == p.Part {
			chain = chain[:i]
			break
		}
	}
	step.chains[p.CCD] = append(chain, &checkingChain{start: p, seq: e.Seq})
	g.lastSeq = e.Seq
	s.projectionDirty = true
}

func (s *State) recordCheckingTrial(p *journal.TrialIntent) {
	step := s.checking.partial[p.Step]
	if p.Cycle != s.checking.cycle || step == nil || p.Regime != machine.R7 {
		return
	}
	for _, chain := range step.chains {
		for _, node := range chain {
			if node.start.Workload == p.Workload && slices.Equal(node.start.Cores, p.Cores) {
				node.started = true
			}
		}
	}
}

// partialRequirements serves the legacy dashboard projection, not R7 scheduling.
func (s *State) partialRequirements(full requirement, cores []int) []requirement {
	short := full.class.withDuration(s.durations.ShortTrialS)
	short.cores = coresKey(cores)
	long := short.withDuration(s.longS(full.cores))
	if short == long {
		return []requirement{{class: short, cores: cores, count: 4}}
	}
	return []requirement{{class: short, cores: cores, count: 3}, {class: long, cores: cores, count: 1}}
}

func (s *State) stepWorkload(step int) string {
	r := s.checking.steps[step]
	occurrence := 0
	for i := range step {
		if s.checking.steps[i] == r {
			occurrence++
		}
	}
	catalog := machine.Workloads(r)
	return catalog[occurrence%len(catalog)].ID
}

func (s *State) r7StepParts(step int) [][]int {
	var out [][]int
	started := s.checking.partial[step+1]
	for _, full := range s.ccdParts() {
		out = append(out, full)
		if started != nil {
			for _, node := range started.chains[s.ccd[full[0]]] {
				if len(node.start.Cores) >= 2 && (node.started || slices.Equal(node.start.Profile, s.checking.profile)) {
					out = append(out, node.start.Cores)
				}
			}
		}
	}
	if len(s.parts) > 1 {
		out = append(out, s.parts[len(s.parts)-1])
	}
	return out
}

func (s *State) deriveCheckingChain(step, ccd, index int, previous []int) Action {
	g := &s.checking
	w := s.stepWorkload(step)
	voltages, sources := s.r7Requests(w, previous, g.profile)
	groups := requests.Groups(voltages)
	var cores []int
	var top []int
	if len(groups) > 0 {
		top = groups[0]
		for _, id := range previous {
			if !slices.Contains(groups[0], id) {
				cores = append(cores, id)
			}
		}
	}
	if len(cores) < 2 {
		cores = nil
	}
	order := fmt.Sprintf("measurements %v", sources)
	if len(sources) == 0 {
		order = "offset fallback (no request telemetry)"
	}
	msg := fmt.Sprintf("cycle %d step %d CCD %d %s: partial %d idles top group %v from %s; loads %v", g.cycle, step+1, ccd, w, index+1, top, order, cores)
	if len(cores) == 0 {
		msg += "; chain ends because removing the top group leaves fewer than two cores"
	}
	p := &journal.CheckingChain{Cycle: g.cycle, Step: step+1, CCD: ccd, Workload: w, Part: fmt.Sprintf("partial %d", index+1), Groups: groups, Cores: cores, SourceSeqs: sources, Profile: slices.Clone(g.profile), Msg: msg}
	cause := append([]int{s.checking.partial[step+1].seq, g.lastSeq}, sources...)
	return Action{Kind: Decide, Payload: p, Cause: cause}
}

func (s *State) r7PartNext(step int, part []int, duration int) (Action, bool) {
	g := &s.checking
	for _, q := range s.requirements(step) {
		if !slices.Equal(q.cores, part) || q.count == 0 || s.passes(q.class, g.profile, g.startSeq, cycleEvidence) >= q.count {
			continue
		}
		// The partial uses its full CCD's duration, not the all-core duration.
		if q.class.duration != s.durations.ShortTrialS && q.class.duration != duration {
			continue
		}
		t := Trial{Regime: machine.R7, Workload: q.class.workload, Cores: slices.Clone(part), DurationS: q.class.duration, Phase: journal.PhaseChecking, Condition: machine.Together, Cycle: g.cycle, Step: step+1}
		if s.retry != nil && s.retry.Cycle == g.cycle && s.retry.Step == step+1 && s.retry.Workload == t.Workload && s.retry.DurationS == t.DurationS && slices.Equal(s.retry.Cores, part) {
			t = *s.retry
		}
		return Action{Kind: RunTrial, Trial: t, Cause: []int{g.lastSeq}}, true
	}
	return Action{}, false
}

func (s *State) r7StepNext(step int) (Action, bool) {
	started := s.checking.partial[step+1]
	if started == nil {
		return Action{Kind: Decide, Payload: s.startCheckingStep(step), Cause: []int{s.checking.lastSeq}}, true
	}
	for _, full := range s.ccdParts() {
		previous := full
		ccd := s.ccd[full[0]]
		chain := started.chains[ccd]
		for index := 0; ; index++ {
			if a, pending := s.r7PartNext(step, previous, s.longS(full)); pending {
				return a, true
			}
			if index == len(chain) || !chain[index].started && !slices.Equal(chain[index].start.Profile, s.checking.profile) {
				return s.deriveCheckingChain(step, ccd, index, previous), true
			}
			previous = chain[index].start.Cores
			if len(previous) == 0 {
				break
			}
		}
	}
	if len(s.parts) > 1 {
		return s.r7PartNext(step, s.parts[len(s.parts)-1], s.longS(s.parts[len(s.parts)-1]))
	}
	return Action{}, false
}

func (s *State) r7ChainsComplete(step int) bool {
	started := s.checking.partial[step+1]
	if started == nil {
		return false
	}
	for _, full := range s.ccdParts() {
		chain := started.chains[s.ccd[full[0]]]
		if len(chain) == 0 {
			return false
		}
		for _, node := range chain {
			if !node.started && !slices.Equal(node.start.Profile, s.checking.profile) {
				return false
			}
		}
		if len(chain[len(chain)-1].start.Cores) != 0 {
			return false
		}
	}
	return true
}
