package main

import (
	"math"
	"slices"
	"time"

	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/requests"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/tuner"
	"github.com/shgew/togi/tools/modelcheck"
	"github.com/shgew/togi/tools/trialfacts"
)

type result struct {
	Scenario                string                     `json:"scenario"`
	Machine                 string                     `json:"machine"`
	Seed                    uint64                     `json:"seed"`
	Split                   string                     `json:"split"`
	Commit                  string                     `json:"commit"`
	Dirty                   bool                       `json:"dirty"`
	Ruleset                 int                        `json:"ruleset"`
	Status                  string                     `json:"status"`
	ModelCheck              *modelcheck.Result         `json:"model_check,omitempty"`
	ExitCode                int                        `json:"exit_code"`
	WallS                   float64                    `json:"wall_s"`
	SimHours                float64                    `json:"sim_hours"`
	FirstPassedCycleH       *float64                   `json:"first_passed_cycle_h"`
	PassedCycles            int                        `json:"passed_cycles"`
	Crashes                 int                        `json:"crashes"`
	Trials                  int                        `json:"trials"`
	RealAnswers             int                        `json:"real_answers"`
	RealAnswerShare         float64                    `json:"real_answer_share"`
	ScenarioRealAnswerShare float64                    `json:"scenario_real_answer_share"`
	TrialHours              float64                    `json:"trial_hours"`
	Hunts                   int                        `json:"hunts"`
	Combinations            int                        `json:"combinations"`
	FinalProfile            []int                      `json:"final_profile"`
	Depth                   int                        `json:"depth"`
	HazardPerH              map[machine.Regime]float64 `json:"hazard_per_h"`
	HazardMaxPerH           float64                    `json:"hazard_max_per_h"`
	WorstR7HazardPerH       *float64                   `json:"worst_r7_hazard_per_h,omitempty"`
}

func metrics(events []journal.Event, m *sim.Machine, cores int) result {
	var r result
	var start time.Time
	var st journal.State
	t := tuner.New()
	journal.Replay(events, &st, t)
	t.Project(&st)
	r.FinalProfile = t.Profile()
	if len(r.FinalProfile) == 0 {
		r.FinalProfile = make([]int, cores)
		if st.Session != nil {
			copy(r.FinalProfile, st.Session.Baseline)
		}
	}
	for _, offset := range r.FinalProfile {
		r.Depth += offset
	}
	for _, e := range events {
		if e.Kind == journal.KindSessionStart && start.IsZero() {
			start = e.Time
		}
		if start.IsZero() {
			continue
		}
		h := e.Time.Sub(start).Hours()
		r.SimHours = h
		switch p := e.Data.(type) {
		case *journal.CheckingCycle:
			if p.Event == journal.CycleEnd && p.Passed {
				r.PassedCycles++
				if r.FirstPassedCycleH == nil {
					r.FirstPassedCycleH = new(h)
				}
			}
		case *journal.CrashDetected:
			r.Crashes++
		case *journal.TrialEnd:
			r.Trials++
			r.TrialHours += float64(p.DurationS) / 3600
		case *journal.HuntStart:
			r.Hunts++
		case *journal.Combination:
			r.Combinations++
		}
	}
	for _, trial := range facts.FromEvents(events).Trials {
		if !trial.Started || trial.End == nil || (trial.End.Outcome != journal.OutcomePass && trial.End.Outcome != journal.OutcomeFailure) {
			continue
		}
		intent := trial.Intent
		if m.HasRealAnswer(intent.Profile, trialfacts.Spec(facts.ClassOf(intent))) {
			r.RealAnswers++
		}
	}
	r.RealAnswerShare = answerShare(r.RealAnswers, r.Trials)
	loaded := make([]int, cores)
	for i := range loaded {
		loaded[i] = i
	}
	r.HazardPerH = make(map[machine.Regime]float64, len(machine.Regimes))
	for _, regime := range machine.Regimes {
		rate := m.Hazard(r.FinalProfile, machine.TrialSpec{Regime: regime, Cores: loaded}) * 3600
		r.HazardPerH[regime] = rate
		r.HazardMaxPerH = max(r.HazardMaxPerH, rate)
	}
	r.WorstR7HazardPerH = worstR7HazardPerH(m, r.FinalProfile)
	return r
}

func worstR7HazardPerH(m *sim.Machine, profile []int) *float64 {
	var all [16]int
	for core := range all {
		all[core] = core
	}
	spec := machine.TrialSpec{Regime: machine.R7}
	var worst float64
	for _, workload := range machine.Workloads(machine.R7) {
		spec.Workload = workload
		for ccd := range 2 {
			var loaded [8]int
			for i := range loaded {
				loaded[i] = ccd*8 + i
			}
			spec.Cores = loaded[:]
			for len(spec.Cores) >= 2 {
				lanes, ok := m.R7Requests(profile, spec)
				if !ok {
					return nil
				}
				worst = max(worst, m.Hazard(profile, spec)*3600)
				spec.Cores = nextR7Partial(spec.Cores, lanes)
			}
		}
		spec.Cores = all[:]
		worst = max(worst, m.Hazard(profile, spec)*3600)
	}
	return &worst
}

// nextR7Partial compacts the load in place, dropping the current top tie group.
func nextR7Partial(cores []int, lanes [16]float32) []int {
	top := math.Inf(-1)
	for _, core := range cores {
		top = max(top, float64(lanes[core]))
	}
	n := 0
	for _, core := range cores {
		if float64(lanes[core]) < top-requests.TieV {
			cores[n] = core
			n++
		}
	}
	return cores[:n]
}

func runStatus(exit int, timedOut bool, events []journal.Event, log string) string {
	if timedOut {
		return "timeout"
	}
	if exit == 0 && len(events) > 0 {
		return "concluded"
	}
	if exit == 1 && (slices.ContainsFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindDeadEnd }) || containsDeadEnd(log)) {
		return "deadend"
	}
	if exit == 3 && len(events) > 0 {
		return "censored"
	}
	return "error"
}

func answerShare(real, trials int) float64 {
	if trials == 0 {
		return 0
	}
	return float64(real) / float64(trials)
}

func setScenarioShares(results []result) {
	type counts struct{ real, trials int }
	totals := make(map[string]counts)
	for _, r := range results {
		c := totals[r.Scenario]
		c.real += r.RealAnswers
		c.trials += r.Trials
		totals[r.Scenario] = c
	}
	for i := range results {
		c := totals[results[i].Scenario]
		results[i].ScenarioRealAnswerShare = answerShare(c.real, c.trials)
	}
}
