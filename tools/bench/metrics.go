package main

import (
	"slices"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/tuner"
)

type result struct {
	Scenario            string                     `json:"scenario"`
	Seed                uint64                     `json:"seed"`
	Split               string                     `json:"split"`
	Commit              string                     `json:"commit"`
	Dirty               bool                       `json:"dirty"`
	Ruleset             int                        `json:"ruleset"`
	Status              string                     `json:"status"`
	ModelCheck          *modelCheck                `json:"model_check,omitempty"`
	ExitCode            int                        `json:"exit_code"`
	WallS               float64                    `json:"wall_s"`
	SimHours            float64                    `json:"sim_hours"`
	FirstCleanRotationH *float64                   `json:"first_clean_rotation_h"`
	Crashes             int                        `json:"crashes"`
	Trials              int                        `json:"trials"`
	TrialHours          float64                    `json:"trial_hours"`
	Hunts               int                        `json:"hunts"`
	JointMarks          int                        `json:"joint_marks"`
	FinalProfile        []int                      `json:"final_profile"`
	Depth               int                        `json:"depth"`
	HazardPerH          map[machine.Regime]float64 `json:"hazard_per_h"`
	HazardMaxPerH       float64                    `json:"hazard_max_per_h"`
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
		case *journal.GuardRotation:
			if p.Clean && r.FirstCleanRotationH == nil {
				r.FirstCleanRotationH = new(h)
			}
		case *journal.CrashDetected:
			r.Crashes++
		case *journal.TrialEnd:
			r.Trials++
			r.TrialHours += float64(p.DurationS) / 3600
		case *journal.HuntStart:
			r.Hunts++
		case *journal.MarkJoint:
			r.JointMarks++
		}
	}
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
	return r
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
	return "error"
}
