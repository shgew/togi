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
			profile[f.state.index(t.Core)] = t.Offset
		}
	}
	index := 0
	if c := f.state.core(t.Core); c != nil {
		index = c.workloadIndex[t.Regime]
	}
	return t.Complete(index, profile)
}

func (f *forecastState) drain(b *ForecastBranch) {
	for len(f.state.cores) > 0 {
		a := f.state.Next()
		switch a.Kind {
		case RunTrial:
			t := a.Trial
			if p, _, err := f.complete(t); err == nil {
				t.Workload = p.Workload
			}
			b.Next = &t
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
			b.Decisions = append(b.Decisions, a.Payload)
			f.fold(a.Payload, a.Cause)
			if _, ok := a.Payload.(*journal.DeadEnd); ok {
				return
			}
		}
	}
}

func judgedCore(s *State, p *journal.TrialIntent) *int {
	cores := slices.Clone(p.Cores)
	if p.Core != nil {
		cores = []int{*p.Core}
	}
	slices.Sort(cores)
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
	if len(cores) == 0 {
		return nil
	}
	return new(cores[0])
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
		out.Next = b.Next
		out.Decisions = b.Decisions
		out.NeedsRanking = b.NeedsRanking
		return out
	}
	core := judgedCore(base.state, p)
	premises := []Premise{IfPass}
	requirement := base.state.Requirement(p)
	remaining := max(1, requirement.Needed-requirement.Passed-requirement.Failed)
	if remaining > 1 {
		premises = append(premises, IfAllPass)
	}
	if core != nil {
		premises = append(premises, IfNamed)
	}
	if p.Condition != machine.Alone {
		end := &journal.TrialEnd{Trial: p.Trial, Outcome: journal.OutcomeFailure, Signal: machine.ComputationError}
		if p.RecordOnly || base.state.attributeTogether(&awaiting{intent: p, end: end}).Attribution == journal.Unattributed {
			premises = append(premises, IfUnnamed)
		}
	}
	premises = append(premises, IfInconclusive)
	for _, premise := range premises {
		f := replayForecast(events)
		b := ForecastBranch{Premise: premise}
		if premise == IfNamed {
			b.Core = core
		}
		if premise == IfPass {
			b.Passes = 1
		}
		if premise == IfAllPass {
			b.Passes = remaining
		}
		f.end(p, premise, core)
		f.drain(&b)
		if premise == IfAllPass {
			for n := 1; n < remaining && b.Next != nil && !b.NeedsRanking; n++ {
				intent, _, err := f.complete(*b.Next)
				if err != nil {
					break
				}
				intent.Trial = fmt.Sprintf("forecast-%d", f.seq+1)
				f.fold(intent, nil)
				f.end(intent, IfPass, nil)
				b.Next = nil
				f.drain(&b)
			}
		}
		out.Branches = append(out.Branches, b)
	}
	return out
}
