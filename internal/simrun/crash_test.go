package simrun

import (
	"context"
	"testing"
	"time"

	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
)

func TestResetReasonPowerButtonAndThermal(t *testing.T) {
	for _, tc := range []struct {
		name  string
		reset machine.ResetKind
		want  machine.Signal
		dead  journal.DeadEndCondition
	}{
		{"power button in trial", machine.ResetPowerButton, machine.Crash, ""},
		{"thermal trip", machine.ResetThermalTrip, "", journal.DeadEndThermalTrip},
	} {
		t.Run(tc.name, func(t *testing.T) {
			cfg := huntConfig(4)
			cfg.Script = map[string]sim.Outcome{"0001": {Signal: machine.Crash, Reset: tc.reset}}
			stop, events, _ := runHunt(t, cfg, nil, nil)
			crash, ok := findPayload(events, func(p *journal.CrashDetected) bool { return p.ResetReason == tc.reset })
			if !ok {
				t.Fatalf("no crash with reset reason %s", tc.reset)
			}
			if tc.dead != "" {
				if stop.Reason != session.StopDeadEnd || stop.DeadEnd.Condition != tc.dead {
					t.Fatalf("stop %+v", stop)
				}
				if !crash.Inconclusive {
					t.Fatal("thermal trip recorded as tuning failure")
				}
				return
			}
			if crash.Inconclusive {
				t.Fatal("power button during trial was inconclusive")
			}
			if _, ok := findPayload(events, func(p *journal.Failure) bool { return p.Signal == tc.want }); !ok {
				t.Fatal("power-button crash has no failure")
			}
		})
	}
}

func TestThermalTripWhileTrialRunningDeadEnds(t *testing.T) {
	t.Parallel()
	fired := false
	var m *sim.Machine
	stop, events, _ := runHunt(t, huntConfig(4), func(machine *sim.Machine) { m = machine }, func(in *Input) {
		in.Wrap = func(j session.Journal) session.Journal {
			return &resetAtEvent{Journal: j, machine: m, kind: journal.KindTrialStart, reset: machine.ResetThermalTrip, fired: &fired}
		}
	})
	if !fired || stop.Reason != session.StopDeadEnd || stop.DeadEnd == nil || stop.DeadEnd.Condition != journal.DeadEndThermalTrip {
		t.Fatalf("thermal trip fired %t, stop %+v", fired, stop)
	}
	if p, ok := findPayload(events, func(p *journal.CrashDetected) bool {
		return p.ResetReason == machine.ResetThermalTrip && p.Inconclusive
	}); !ok || p.InFlight == nil {
		t.Fatalf("missing in-flight thermal crash: %+v", p)
	}
	if _, ok := findPayload(events, func(p *journal.Failure) bool { return true }); ok {
		t.Fatal("thermal trip recorded as tuning failure")
	}
}

func TestPowerButtonBetweenTrialsIsInconclusive(t *testing.T) {
	t.Parallel()
	fired := false
	var m *sim.Machine
	_, events, _ := runHunt(t, huntConfig(4), func(machine *sim.Machine) { m = machine }, func(in *Input) {
		in.Wrap = func(j session.Journal) session.Journal {
			return &resetAtEvent{Journal: j, machine: m, kind: journal.KindTrialEnd, reset: machine.ResetPowerButton, fired: &fired}
		}
		in.Until = func(e journal.Event) bool {
			p, ok := e.Data.(*journal.CrashDetected)
			return ok && p.ResetReason == machine.ResetPowerButton
		}
	})
	if !fired {
		t.Fatal("no between-trials power-button crash")
	}
	crash, ok := findPayload(events, func(p *journal.CrashDetected) bool { return p.ResetReason == machine.ResetPowerButton })
	if !ok || !crash.Inconclusive {
		t.Fatalf("power-button crash %+v, want inconclusive", crash)
	}
	if _, ok := findPayload(events, func(p *journal.Failure) bool { return true }); ok {
		t.Fatal("power-button crash between trials recorded a tuning failure")
	}
}

type jumpAtTrialStart struct {
	session.Journal
	machine *sim.Machine
	jumped  bool
}

func (j *jumpAtTrialStart) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := j.Journal.Append(p, cause...)
	if err == nil && !j.jumped && e.Kind == journal.KindTrialStart {
		j.machine.JumpWall(-2 * time.Hour)
		j.jumped = true
	}
	return e, err
}

func TestClockJumpDoesNotLoseTrialMCE(t *testing.T) {
	t.Parallel()
	cfg := huntConfig(4)
	cfg.Script = map[string]sim.Outcome{"0001": {Signal: machine.CorrectedMCE, AtS: 2, Core: 0}}
	m, err := sim.New(cfg)
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	jump := &jumpAtTrialStart{machine: m}
	in := Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Cycles: 1,
		Wrap:  func(j session.Journal) session.Journal { jump.Journal = j; return jump },
		Until: func(e journal.Event) bool { return e.Kind == journal.KindTrialEnd },
	}
	if _, err := Simulate(context.Background(), in); err != nil {
		t.Fatal(err)
	}
	events, _, err := journal.Read(dir)
	if err != nil {
		t.Fatal(err)
	}
	if !jump.jumped {
		t.Fatal("wall clock was not moved after trial start")
	}
	start, ok := findPayload(events, func(p *journal.TrialIntent) bool { return p.Trial == "0001" })
	if !ok {
		t.Fatal("missing scripted trial")
	}
	end, ok := findPayload(events, func(p *journal.TrialEnd) bool { return p.Trial == start.Trial })
	if !ok || end.Outcome != journal.OutcomeFailure || end.Signal != machine.CorrectedMCE {
		t.Fatalf("trial end %+v, want corrected MCE failure", end)
	}
	if _, ok := findPayload(events, func(p *journal.MCE) bool { return p.Corrected }); !ok {
		t.Fatal("corrected machine check was lost after the wall clock jump")
	}
}
