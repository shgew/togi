package tuner

import (
	"fmt"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"slices"
)

type forecastState struct {
	state   *State
	seq     int
	ranking *journal.HostRanking
}

func replayForecast(events []journal.Event) forecastState {
	f := forecastState{state: New()}
	for _, e := range events {
		f.state.Fold(e)
		f.seq = max(f.seq, e.Seq)
		if p, ok := e.Data.(*journal.HostRanking); ok {
			f.ranking = p
		}
	}
	return f
}

func (f *forecastState) fold(p journal.Payload, cause []int) {
	f.seq++
	f.state.Fold(journal.Event{Seq: f.seq, Kind: p.Kind(), Msg: p.Message(), Data: p, Cause: slices.Clone(cause)})
}

func (f *forecastState) complete(t Trial) (*journal.TrialIntent, machine.Workload, error) {
	profile := t.Profile
	if profile == nil {
		profile = f.state.offsets()
		if t.Condition == machine.Alone {
			profile = make([]int, len(profile))
			if i := f.state.index(t.Core); i >= 0 && i < len(profile) {
				profile[i] = t.Offset
			}
		}
	}
	index := 0
	if c := f.state.core(t.Core); c != nil {
		index = c.workloadIndex[t.Regime]
	}
	return t.Complete(index, profile)
}

// forecastSteps bounds the decisions one branch may fold before its next trial. Every real path needs far fewer;
// reaching it means the replayed state cannot settle without evidence the journal lacks.
const forecastSteps = 1000

func (f *forecastState) drain(b *ForecastBranch) {
	b.NextStep = 0
	var plans []*journal.HuntGroup
	for steps := 0; len(f.state.cores) > 0; steps++ {
		if steps == forecastSteps {
			b.NeedsHistory = true
			return
		}
		if f.state.huntNeedsProfile() {
			b.NeedsHistory = true
			return
		}
		a := f.state.Next()
		switch a.Kind {
		case RunTrial:
			t := a.Trial
			if p, _, err := f.complete(t); err == nil {
				t.Workload = p.Workload
			}
			b.Next = &t
			b.NextStep = t.Step
			if t.Step == 0 && t.Phase == journal.PhaseChecking && t.Cycle > 0 && !t.Rerun && t.Hunt == 0 {
				if plan := f.state.CyclePlan(); plan.Number == t.Cycle && plan.Current < len(plan.Steps) {
					b.NextStep = plan.Current + 1
				}
			}
			return
		case ReadRanking:
			if f.ranking == nil {
				b.NeedsRanking = true
				return
			}
			p := *f.ranking
			p.Ranking = slices.Clone(p.Ranking)
			f.fold(&p, a.Cause)
		case Decide:
			if group, ok := a.Payload.(*journal.HuntGroup); ok {
				if slices.ContainsFunc(plans, func(prior *journal.HuntGroup) bool { return sameHuntPlan(prior, group) }) {
					b.NeedsHistory = true
					return
				}
				plans = append(plans, group)
			}
			b.Decisions = append(b.Decisions, a.Payload)
			f.fold(a.Payload, a.Cause)
			if _, ok := a.Payload.(*journal.DeadEnd); ok {
				return
			}
		}
	}
}

func (s *State) huntNeedsProfile() bool {
	h := s.hunt
	if h == nil || len(s.checking.profile) == len(s.cores) {
		return false
	}
	return h.end != nil || len(h.groups) > 0 && s.groupOutcome(h, h.groups[len(h.groups)-1]) != "running"
}

func sameHuntPlan(a, b *journal.HuntGroup) bool {
	probe := a.Probe == nil && b.Probe == nil || a.Probe != nil && b.Probe != nil && *a.Probe == *b.Probe
	return a.Hunt == b.Hunt && a.DurationS == b.DurationS && a.Granularity == b.Granularity &&
		a.Stage == b.Stage && a.Index == b.Index && a.Escalated == b.Escalated &&
		a.FullChecked == b.FullChecked && a.AnyFailed == b.AnyFailed &&
		a.Inferred == b.Inferred && a.Skipped == b.Skipped && probe &&
		slices.Equal(a.Cores, b.Cores) && slices.Equal(a.Set, b.Set) &&
		slices.Equal(a.Profile, b.Profile) && slices.Equal(a.Held, b.Held)
}

// namedCores returns the cores whose naming the forecast follows: a judged core away from 0 standing in for every
// core whose failure backs it off, then the first loaded core at 0, judged or parked, whose failure is a dead end.
func namedCores(s *State, p *journal.TrialIntent) []int {
	cores := slices.Clone(p.Cores)
	if p.Core != nil {
		cores = []int{*p.Core}
	}
	slices.Sort(cores)
	loaded := slices.Clone(cores)
	if s.hunt != nil && p.Hunt == s.hunt.start.Hunt {
		for _, g := range s.hunt.groups {
			if g.payload.Group == p.Group {
				cores = slices.DeleteFunc(cores, func(id int) bool {
					return !slices.Contains(g.payload.Cores, id) && (g.payload.Probe == nil || g.payload.Probe.Core != id)
				})
				break
			}
		}
	}
	atZero := func(id int) bool {
		i := s.index(id)
		return i < 0 || i >= len(p.Profile) || p.Profile[i] == 0
	}
	if len(cores) == 0 {
		// A group's parked cores can still carry load and be named.
		cores = loaded
	}
	if len(cores) == 0 {
		return nil
	}
	standIn := cores[0]
	if i := slices.IndexFunc(cores, func(id int) bool { return !atZero(id) }); i >= 0 {
		standIn = cores[i]
	}
	named := []int{standIn}
	// Any loaded core at 0, judged or parked, ends tuning when named.
	if i := slices.IndexFunc(loaded, func(id int) bool { return id != standIn && atZero(id) }); i >= 0 {
		named = append(named, loaded[i])
	}
	return named
}

// sameRequirement reports whether a forecast trial repeats the in-flight trial's evidence requirement.
func sameRequirement(a, b *journal.TrialIntent) bool {
	return classOf(a) == classOf(b) && slices.Equal(a.Profile, b.Profile) && a.Phase == b.Phase &&
		a.Condition == b.Condition && a.RecordOnly == b.RecordOnly && a.Rerun == b.Rerun &&
		a.Cycle == b.Cycle && a.Step == b.Step && a.Hunt == b.Hunt && a.Group == b.Group && a.Round == b.Round
}

func (f *forecastState) end(p *journal.TrialIntent, premise Premise, core *int) {
	e := &journal.TrialEnd{Trial: p.Trial, DurationS: p.DurationS, Outcome: journal.OutcomePass}
	switch premise {
	case IfPass, IfAllPass:
	case IfNamed:
		e.Outcome = journal.OutcomeFailure
		e.Signal = machine.ComputationError
		e.Core = core
	case IfUnnamed:
		e.Outcome = journal.OutcomeFailure
		e.Signal = machine.ComputationError
	case IfInconclusive:
		e.Outcome = journal.OutcomeInconclusive
		e.DurationS = 0
	}
	f.fold(e, nil)
}

// Forecast replays events independently for each premise, without hardware, clocks or randomness.
// Recorded rankings answer future reads; NeedsRanking marks a read without recorded evidence.
// Until every core's initial phase is recorded, there is no schedulable forecast.
func Forecast(events []journal.Event) ForecastPlan {
	base := replayForecast(events)
	out := ForecastPlan{}
	if len(base.state.cores) == 0 || slices.ContainsFunc(base.state.cores, func(c *core) bool { return c.phase == "" }) {
		return out
	}
	if len(events) > 0 {
		switch events[len(events)-1].Data.(type) {
		case *journal.Shutdown, *journal.DeadEnd:
			return out
		}
	}
	p := base.state.inFlight()
	if p == nil {
		b := ForecastBranch{}
		base.drain(&b)
		out.Next, out.NextStep = b.Next, b.NextStep
		out.Decisions = b.Decisions
		out.NeedsRanking = b.NeedsRanking
		out.NeedsHistory = b.NeedsHistory
		return out
	}
	type premiseCore struct {
		premise Premise
		core    *int
	}
	premises := []premiseCore{{premise: IfPass}}
	requirement := base.state.Requirement(p)
	remaining := max(1, requirement.Needed-requirement.Passed-requirement.Failed)
	if remaining > 1 {
		premises = append(premises, premiseCore{premise: IfAllPass})
	}
	for _, id := range namedCores(base.state, p) {
		premises = append(premises, premiseCore{premise: IfNamed, core: new(id)})
	}
	if p.Condition != machine.Alone {
		end := &journal.TrialEnd{Trial: p.Trial, Outcome: journal.OutcomeFailure, Signal: machine.ComputationError}
		if p.RecordOnly || base.state.attributeTogether(&awaiting{intent: p, end: end}).Attribution == journal.Unattributed {
			premises = append(premises, premiseCore{premise: IfUnnamed})
		}
	}
	premises = append(premises, premiseCore{premise: IfInconclusive})
	for _, pc := range premises {
		premise := pc.premise
		f := replayForecast(events)
		b := ForecastBranch{Premise: premise, Core: pc.core}
		if premise == IfPass {
			b.Passes = 1
		}
		if premise == IfAllPass {
			b.Passes = remaining
		}
		f.end(p, premise, pc.core)
		f.drain(&b)
		if premise == IfAllPass && !f.passRemaining(p, remaining, &b) {
			// Another requirement's trial comes first, so the premise cannot hold on its own.
			continue
		}
		out.Branches = append(out.Branches, b)
	}
	return out
}

// passRemaining folds passes of the in-flight requirement's remaining trials. It reports false when the tuner
// schedules another requirement's trial before they all ran.
func (f *forecastState) passRemaining(p *journal.TrialIntent, remaining int, b *ForecastBranch) bool {
	for n := 1; n < remaining && b.Next != nil && !b.NeedsRanking && !b.NeedsHistory; n++ {
		intent, _, err := f.complete(*b.Next)
		if err != nil {
			break
		}
		if !sameRequirement(p, intent) {
			return false
		}
		intent.Trial = fmt.Sprintf("forecast-%d", f.seq+1)
		f.fold(intent, nil)
		f.end(intent, IfPass, nil)
		b.Next = nil
		f.drain(b)
	}
	return true
}
