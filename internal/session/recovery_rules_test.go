package session

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"iter"
	"os"
	"path/filepath"
	"slices"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/trial"
)

func firstCrash(t *testing.T, reset machine.ResetKind, signal machine.Signal, thenCrash bool) (simRun, []journal.Event) {
	t.Helper()
	cfg := small()
	cfg.Script = map[string]sim.Outcome{"0001": {Signal: signal, Reset: reset, ThenCrash: thenCrash, AtS: 2, Core: 0}}
	model := sim.DefaultModel()
	model.CrashMCE = 0
	cfg.Model = &model
	m := newSim(t, cfg)
	in := simInput(t.TempDir(), m)
	if reset == machine.ResetPowerLoss {
		m.NextReset(machine.ResetWatchdog)
		if _, err := simulateBoot(context.Background(), in, wrapFor(in, crashAt(1, m))); !errors.Is(err, machine.ErrCrashed) {
			t.Fatalf("seed known reset: %v", err)
		}
		m.Reboot()
	}
	_, err := simulateBoot(context.Background(), in, wrapFor(in, nil))
	if !errors.Is(err, machine.ErrCrashed) {
		t.Fatalf("first trial: %v, want crash", err)
	}
	m.Reboot()
	return in, readEvents(t, in.Dir)
}

func TestTrialResetClassification(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		reset   machine.ResetKind
		outcome journal.Outcome
		signal  machine.Signal
		deadEnd journal.DeadEndCondition
	}{
		{"watchdog", machine.ResetWatchdog, journal.OutcomeFailure, machine.Crash, ""},
		{"sync flood", machine.ResetSyncFlood, journal.OutcomeFailure, machine.Crash, ""},
		{"CPU shutdown", machine.ResetCPUShutdown, journal.OutcomeFailure, machine.Crash, ""},
		{"power button during trial", machine.ResetPowerButton, journal.OutcomeFailure, machine.Crash, ""},
		{"power loss", machine.ResetPowerLoss, journal.OutcomeInconclusive, "", ""},
		{"thermal trip", machine.ResetThermalTrip, journal.OutcomeInconclusive, "", journal.DeadEndThermalTrip},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			in, _ := firstCrash(t, tc.reset, machine.Crash, false)
			stop := simulate(t, in)
			if tc.deadEnd != "" && (stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != tc.deadEnd) {
				t.Fatalf("stop %+v, want %s", stop, tc.deadEnd)
			}
			events := readEvents(t, in.Dir)
			var detected *journal.CrashDetected
			var end *journal.TrialEnd
			var failures int
			for _, e := range events {
				switch p := e.Data.(type) {
				case *journal.CrashDetected:
					if detected == nil {
						detected = p
					}
				case *journal.TrialEnd:
					if p.Trial == "0001" {
						end = p
					}
				case *journal.Failure:
					if p.Trial == "0001" {
						failures++
					}
				}
			}
			if tc.reset == machine.ResetPowerLoss {
				detected = nil
			}
			for _, e := range events {
				if p, ok := e.Data.(*journal.CrashDetected); ok && p.InFlight != nil && detected == nil {
					detected = p
				}
			}
			if detected == nil || end == nil {
				t.Fatalf("missing crash/end: detected %+v, end %+v", detected, end)
			}
			if detected.ResetReason != tc.reset && (tc.reset != machine.ResetPowerLoss || detected.ResetReason != "") {
				t.Fatalf("reset reason %q, want %q", detected.ResetReason, tc.reset)
			}
			if end.Outcome != tc.outcome || end.Signal != tc.signal {
				t.Fatalf("trial end %+v, want %s / %s", end, tc.outcome, tc.signal)
			}
			wantFailures := 0
			if tc.outcome == journal.OutcomeFailure {
				wantFailures = 1
			}
			if failures != wantFailures {
				t.Fatalf("%d failures for crashed trial, want %d", failures, wantFailures)
			}
			if detected.Stray {
				t.Fatal("trial reset classified stray")
			}
			if detected.Inconclusive != (tc.outcome == journal.OutcomeInconclusive) {
				t.Fatalf("inconclusive=%v, want %v", detected.Inconclusive, tc.outcome == journal.OutcomeInconclusive)
			}
		})
	}
}

func TestTrialResetReasonAcrossNonTogiBoot(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		crash   machine.ResetKind
		later   machine.ResetKind
		outcome journal.Outcome
		deadEnd journal.DeadEndCondition
	}{
		{"later thermal trip cannot replace watchdog", machine.ResetWatchdog, machine.ResetThermalTrip, journal.OutcomeFailure, ""},
		{"later clean reboot cannot hide thermal trip", machine.ResetThermalTrip, machine.ResetUnknown, journal.OutcomeInconclusive, journal.DeadEndThermalTrip},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			in, before := firstCrash(t, tc.crash, machine.Crash, false)
			in.Machine.NextReset(tc.later)
			in.Machine.Reboot()
			stop := simulate(t, in)
			if tc.deadEnd != "" && (stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != tc.deadEnd) {
				t.Fatalf("stop %+v, want %s", stop, tc.deadEnd)
			}
			events := readEvents(t, in.Dir)
			crash, ok := crashDetectedFor(events, before[0].Boot)
			if !ok {
				t.Fatal("missing crash detection")
			}
			if got := crash.Data.(*journal.CrashDetected).ResetReason; got != tc.crash {
				t.Fatalf("reset reason %q, want %q", got, tc.crash)
			}
			for _, e := range events {
				if end, ok := e.Data.(*journal.TrialEnd); ok && end.Trial == "0001" {
					if end.Outcome != tc.outcome {
						t.Fatalf("outcome %s, want %s", end.Outcome, tc.outcome)
					}
					return
				}
			}
			t.Fatal("missing recovered trial end")
		})
	}
}

func TestIdleResetReasonAcrossNonTogiBoot(t *testing.T) {
	t.Parallel()
	_, ref := reference(t, small())
	index := slices.IndexFunc(ref, func(e journal.Event) bool { return e.Kind == journal.KindTrialEnd })
	if index < 0 {
		t.Fatal("missing first trial end")
	}
	for _, tc := range []struct {
		name         string
		crash, later machine.ResetKind
		inconclusive bool
	}{
		{"later power button cannot excuse watchdog", machine.ResetWatchdog, machine.ResetPowerButton, false},
		{"later watchdog cannot blame power button", machine.ResetPowerButton, machine.ResetWatchdog, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			in := simInput(t.TempDir(), newSim(t, small()))
			in.Machine.NextReset(tc.crash)
			_, err := simulateBoot(context.Background(), in, wrapFor(in, crashAt(ref[index].Seq, in.Machine)))
			if !errors.Is(err, machine.ErrCrashed) {
				t.Fatalf("between-trial crash: %v", err)
			}
			in.Machine.Reboot()
			in.Machine.NextReset(tc.later)
			in.Machine.Reboot()
			simulate(t, in)
			events := readEvents(t, in.Dir)
			crash, ok := crashDetectedFor(events, ref[index].Boot)
			if !ok {
				t.Fatal("missing crash detection")
			}
			p := crash.Data.(*journal.CrashDetected)
			if p.ResetReason != tc.crash || p.Inconclusive != tc.inconclusive {
				t.Fatalf("idle crash: %+v, want %s, inconclusive %v", p, tc.crash, tc.inconclusive)
			}
			if got := failureCiting(events, crash.Seq) != nil; got == tc.inconclusive {
				t.Fatalf("idle failure %v, want %v", got, !tc.inconclusive)
			}
		})
	}
}

func TestSignalBeforePowerLossSurvivesCrash(t *testing.T) {
	t.Parallel()
	for _, signal := range []machine.Signal{machine.ComputationError, machine.UnexpectedExit, machine.Stall} {
		t.Run(string(signal), func(t *testing.T) {
			t.Parallel()
			in, before := firstCrash(t, machine.ResetPowerLoss, signal, true)
			var progress *journal.TrialProgress
			for _, e := range before {
				switch p := e.Data.(type) {
				case *journal.TrialProgress:
					if p.Trial == "0001" && p.Signal != "" {
						progress = p
					}
				case *journal.TrialEnd:
					if p.Trial == "0001" {
						t.Fatal("trial ended before crash")
					}
				}
			}
			if progress == nil || progress.Signal != signal || progress.Core == nil || *progress.Core != 0 {
				t.Fatalf("durable backend evidence %+v, want %s on core 0", progress, signal)
			}
			stop := simulate(t, in)
			if stop.Reason != StopCycles {
				t.Fatalf("stopped with %+v", stop)
			}
			var end *journal.TrialEnd
			var failure *journal.Failure
			for _, e := range readEvents(t, in.Dir) {
				switch p := e.Data.(type) {
				case *journal.TrialEnd:
					if p.Trial == "0001" {
						end = p
					}
				case *journal.Failure:
					if p.Trial == "0001" {
						failure = p
					}
				}
			}
			if end == nil || end.Outcome != journal.OutcomeFailure || end.Signal != signal {
				t.Fatalf("power loss after signal: %+v", end)
			}
			if failure == nil || failure.Signal != signal {
				t.Fatalf("recovered failure: %+v", failure)
			}
			if failure.Attribution != journal.Attributed || failure.Core == nil || *failure.Core != 0 {
				t.Fatalf("failed core: %+v, want attributed core 0", failure)
			}
		})
	}
}

func TestPowerButtonBetweenTrialsIsInconclusive(t *testing.T) {
	t.Parallel()
	_, ref := reference(t, small())
	index := slices.IndexFunc(ref, func(e journal.Event) bool { return e.Kind == journal.KindTrialEnd })
	if index < 0 {
		t.Fatal("missing first trial end")
	}
	in := simInput(t.TempDir(), newSim(t, small()))
	in.Machine.NextReset(machine.ResetPowerButton)
	if stop := drive(t, in, crashAt(ref[index].Seq, in.Machine)); stop.Reason != StopCycles {
		t.Fatalf("stopped with %+v", stop)
	}
	crash, ok := crashDetectedFor(readEvents(t, in.Dir), ref[index].Boot)
	if !ok {
		t.Fatal("no crash detection")
	}
	p := crash.Data.(*journal.CrashDetected)
	if !p.Inconclusive || p.Stray || p.ResetReason != machine.ResetPowerButton {
		t.Fatalf("between-trial crash: %+v", p)
	}
	if failureCiting(readEvents(t, in.Dir), crash.Seq) != nil {
		t.Fatal("between-trial power button counted as failure")
	}
}

func TestTrialProgressAndDurationIgnoreWallJump(t *testing.T) {
	t.Parallel()
	for _, jump := range []time.Duration{-time.Hour, time.Hour} {
		t.Run(jump.String(), func(t *testing.T) {
			t.Parallel()
			in := simInput(t.TempDir(), newSim(t, small()))
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			seams := in.Machine.Seams()
			seams.Trials = wallJumpTrials{Trials: seams.Trials, jump: func() { in.Machine.JumpWall(jump) }, cancel: cancel}
			stop, err := runWithSeams(ctx, in, seams)
			if err != nil || stop.Reason != StopSignal {
				t.Fatalf("stop %+v, %v", stop, err)
			}
			var end *journal.TrialEnd
			for _, e := range readEvents(t, in.Dir) {
				if p, ok := e.Data.(*journal.TrialEnd); ok {
					end = p
					break
				}
			}
			if end == nil || !end.Interrupted || end.Outcome != journal.OutcomeInconclusive {
				t.Fatalf("interrupted trial: %+v", end)
			}
			if diff := cmp.Diff(85, end.DurationS); diff != "" {
				t.Fatalf("interrupted duration (-want +got):\n%s", diff)
			}
		})
	}
}

type wallJumpTrials struct {
	machine.Trials
	jump   func()
	cancel context.CancelFunc
}

func (t wallJumpTrials) Start(ctx context.Context, spec machine.TrialSpec) (machine.Running, error) {
	r, err := t.Trials.Start(ctx, spec)
	if err != nil {
		return nil, err
	}
	return wallJumpRunning{Running: r, jump: t.jump, cancel: t.cancel}, nil
}

type wallJumpRunning struct {
	machine.Running
	jump   func()
	cancel context.CancelFunc
}

func (r wallJumpRunning) Wait(context.Context, machine.Reporter) (machine.Result, error) {
	r.jump()
	r.cancel()
	return machine.Result{Ran: 85 * time.Second}, context.Canceled
}

func (r wallJumpRunning) Stop() error {
	r.jump()
	r.cancel()
	return nil
}

func runWithSeams(ctx context.Context, in simRun, seams machine.Machine) (Stop, error) {
	in.Seams = &seams
	return simulateBoot(ctx, in, wrapFor(in, nil))
}

func TestCorrectedMCESelectionSurvivesWallJump(t *testing.T) {
	t.Parallel()
	for _, jump := range []time.Duration{-time.Hour, time.Hour} {
		t.Run(jump.String(), func(t *testing.T) {
			t.Parallel()
			cfg := small()
			cfg.Script = map[string]sim.Outcome{"0001": {Signal: machine.CorrectedMCE, AtS: 1, Core: 0}}
			in := simInput(t.TempDir(), newSim(t, cfg))
			seams := in.Machine.Seams()
			seams.Trials = jumpTrials{Trials: seams.Trials, jump: func() { in.Machine.JumpWall(jump) }}
			stop, err := driveWithSeams(in, seams)
			if err != nil || stop.Reason != StopCycles {
				t.Fatalf("run %+v, %v", stop, err)
			}
			events := readEvents(t, in.Dir)
			var end journal.Event
			for _, e := range events {
				if p, ok := e.Data.(*journal.TrialEnd); ok && p.Trial == "0001" {
					end = e
					break
				}
			}
			p, ok := end.Data.(*journal.TrialEnd)
			if !ok || p.Outcome != journal.OutcomeFailure || p.Signal != machine.CorrectedMCE {
				t.Fatalf("trial after wall jump: %+v", end.Data)
			}
			if !slices.ContainsFunc(end.Cause, func(seq int) bool { return seq > 0 && seq <= len(events) && events[seq-1].Kind == journal.KindMCE }) {
				t.Fatalf("trial.end did not cite MCE: %v", end.Cause)
			}
		})
	}
}

type jumpTrials struct {
	machine.Trials
	jump func()
}

func (t jumpTrials) Start(ctx context.Context, spec machine.TrialSpec) (machine.Running, error) {
	r, err := t.Trials.Start(ctx, spec)
	if err != nil {
		return nil, err
	}
	return jumpRunning{Running: r, jump: t.jump}, nil
}

type jumpRunning struct {
	machine.Running
	jump func()
}

func (r jumpRunning) Wait(ctx context.Context, report machine.Reporter) (machine.Result, error) {
	r.jump()
	return r.Running.Wait(ctx, report)
}

func (r jumpRunning) Stop() error {
	r.jump()
	return r.Running.Stop()
}

type jumpAndCrashTrials struct {
	machine.Trials
	m     *sim.Machine
	jump  time.Duration
	fired bool
}

func (t *jumpAndCrashTrials) Start(ctx context.Context, spec machine.TrialSpec) (machine.Running, error) {
	r, err := t.Trials.Start(ctx, spec)
	if err != nil {
		return nil, err
	}
	if t.fired {
		return r, nil
	}
	t.fired = true
	return jumpAndCrashRunning{Running: r, m: t.m, jump: t.jump}, nil
}

type jumpAndCrashRunning struct {
	machine.Running
	m    *sim.Machine
	jump time.Duration
}

func (r jumpAndCrashRunning) Wait(ctx context.Context, report machine.Reporter) (machine.Result, error) {
	if err := r.m.Sleep(ctx, 85*time.Second); err != nil {
		return machine.Result{}, err
	}
	r.m.JumpWall(r.jump)
	report.Progress("trial ran 85 seconds")
	r.m.Crash()
	return machine.Result{}, machine.ErrCrashed
}

func (r jumpAndCrashRunning) Stop() error { return nil }

func TestInterruptedTrialDurationUsesMonotonicTimeAfterWallJump(t *testing.T) {
	t.Parallel()
	for _, jump := range []time.Duration{-time.Hour, time.Hour} {
		t.Run(jump.String(), func(t *testing.T) {
			t.Parallel()
			in := simInput(t.TempDir(), newSim(t, small()))
			seams := in.Machine.Seams()
			seams.Trials = &jumpAndCrashTrials{Trials: seams.Trials, m: in.Machine, jump: jump}
			stop, err := driveWithSeams(in, seams)
			if err != nil || stop.Reason != StopCycles {
				t.Fatalf("resume after wall jump %+v, %v", stop, err)
			}
			events := readEvents(t, in.Dir)
			var start, progress, end journal.Event
			for _, e := range events {
				switch p := e.Data.(type) {
				case *journal.TrialStart:
					if p.Trial == "0001" {
						start = e
					}
				case *journal.TrialProgress:
					if p.Trial == "0001" {
						progress = e
					}
				case *journal.TrialEnd:
					if p.Trial == "0001" {
						end = e
					}
				}
			}
			p, ok := end.Data.(*journal.TrialEnd)
			if start.Seq == 0 || progress.Seq == 0 || !ok || p.Outcome != journal.OutcomeFailure || p.Signal != machine.Crash || p.DurationS != 85 {
				t.Fatalf("start mono %d, progress mono %d, end %+v; want 85s crash failure", start.Mono, progress.Mono, end.Data)
			}
		})
	}
}

type sampledTrials struct {
	machine.Trials
	reader *trial.Runner
}

func (t sampledTrials) Samples(id string) iter.Seq[machine.TrialConditions] {
	return t.reader.Samples(id)
}

func TestCrashRecoveryLastSample(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name, contents                string
		seconds, tctl, minMHz, maxMHz *int
		message                       string
	}{
		{"complete", "{\"elapsed_ms\":3700,\"tctl_c\":71,\"core_mhz\":{\"0\":5420,\"1\":5610}}\n", new(3), new(71), new(5420), new(5610), "trial 0001 FAIL crash, last evidence 0s after start, last sample 3s: Tctl 71°C, 5420-5610 MHz"},
		{"torn", "{\"elapsed_ms\":3700,\"tctl_c\":71,\"core_mhz\":{\"0\":5420,\"1\":5610}}\n{\"elapsed_ms\":4800,\"tctl_c\":99}", new(3), new(71), new(5420), new(5610), "trial 0001 FAIL crash, last evidence 0s after start, last sample 3s: Tctl 71°C, 5420-5610 MHz"},
		{"missing sensors", "{\"elapsed_ms\":900}\n", new(0), nil, nil, nil, "trial 0001 FAIL crash, last evidence 0s after start, last sample 0s"},
		{"missing file", "", nil, nil, nil, nil, "trial 0001 FAIL crash, last evidence 0s after start"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			in, _ := firstCrash(t, machine.ResetWatchdog, machine.Crash, false)
			dir := filepath.Join(in.Dir, "trials")
			if tc.contents != "" {
				if err := os.MkdirAll(filepath.Join(dir, "0001"), 0755); err != nil {
					t.Fatal(err)
				}
				if err := os.WriteFile(filepath.Join(dir, "0001", "samples.jsonl"), []byte(tc.contents), 0644); err != nil {
					t.Fatal(err)
				}
			}
			seams := in.Machine.Seams()
			seams.Trials = sampledTrials{Trials: seams.Trials, reader: trial.New(trial.Options{Dir: dir})}
			if _, err := driveWithSeams(in, seams); err != nil {
				t.Fatal(err)
			}
			for _, e := range readEvents(t, in.Dir) {
				if p, ok := e.Data.(*journal.TrialEnd); ok && p.Trial == "0001" {
					want := journal.TrialEnd{LastSampleS: tc.seconds, LastSampleTctlC: tc.tctl, LastSampleMinMHz: tc.minMHz, LastSampleMaxMHz: tc.maxMHz}
					got := journal.TrialEnd{LastSampleS: p.LastSampleS, LastSampleTctlC: p.LastSampleTctlC, LastSampleMinMHz: p.LastSampleMinMHz, LastSampleMaxMHz: p.LastSampleMaxMHz}
					if diff := cmp.Diff(want, got); diff != "" {
						t.Fatal(diff)
					}
					if diff := cmp.Diff(tc.message, e.Msg); diff != "" {
						t.Fatal(diff)
					}
					return
				}
			}
			t.Fatal("missing recovered trial")
		})
	}
}

func TestReplayCrashRecoveryPreservesFact(t *testing.T) {
	t.Parallel()
	probe, events := firstCrash(t, machine.ResetWatchdog, machine.Crash, false)
	bios, err := probe.Machine.Seams().Host.BIOSContext()
	if err != nil {
		t.Fatal(err)
	}
	var intent *journal.TrialIntent
	for _, e := range events {
		if p, ok := e.Data.(*journal.TrialIntent); ok && p.Trial == "0001" {
			intent = p
			break
		}
	}
	if intent == nil {
		t.Fatal("missing first trial intent")
	}
	cores := intent.Cores
	if len(cores) == 0 && intent.Core != nil {
		cores = []int{*intent.Core}
	}
	for _, tc := range []struct {
		name     string
		signal   machine.Signal
		reset    machine.ResetKind
		duration int
	}{
		{"crash exposure", machine.Crash, machine.ResetWatchdog, 7},
		{"crash overrides thermal fallback", machine.Crash, machine.ResetThermalTrip, 7},
		{"uncorrected MCE", machine.UncorrectedMCE, machine.ResetThermalTrip, 7},
		{"zero crash exposure", machine.Crash, machine.ResetWatchdog, 0},
	} {
		t.Run(tc.name, func(t *testing.T) {
			cfg := small()
			model := sim.DefaultModel()
			model.Reset = map[machine.ResetKind]float64{tc.reset: 1}
			cfg.Model = &model
			cfg.Replay, err = sim.NewReplay(bios, []sim.ReplayFact{{
				Context:   bios,
				Class:     journal.TrialClass{Regime: intent.Regime, Workload: intent.Workload, Cores: cores, DurationS: intent.DurationS},
				Profile:   intent.Profile,
				Outcome:   journal.OutcomeFailure,
				Signal:    tc.signal,
				DurationS: tc.duration,
			}})
			if err != nil {
				t.Fatal(err)
			}
			m := newSim(t, cfg)
			in := simInput(t.TempDir(), m)
			if _, err := simulateBoot(context.Background(), in, wrapFor(in, nil)); !errors.Is(err, machine.ErrCrashed) {
				t.Fatalf("replayed trial: %v, want crash", err)
			}
			m.Reboot()
			simulate(t, in)
			for _, e := range readEvents(t, in.Dir) {
				if end, ok := e.Data.(*journal.TrialEnd); ok && end.Trial == "0001" {
					if end.Outcome != journal.OutcomeFailure || end.Signal != tc.signal || end.DurationS != tc.duration {
						t.Fatalf("recovered trial %+v, want failure/%s/%ds", end, tc.signal, tc.duration)
					}
					return
				}
			}
			t.Fatal("missing recovered trial end")
		})
	}
}

func TestReplayMachineCheckKeepsBankAttribution(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name        string
		local       float64
		attribution journal.Attribution
	}{
		{"shared bank", 0, journal.Unattributed},
		{"core-local bank", 1, journal.Attributed},
	} {
		t.Run(tc.name, func(t *testing.T) {
			cfg := small()
			model := sim.DefaultModel()
			model.CoreLocalBank, model.CrashMCE = tc.local, 0
			cfg.Model = &model
			probe := simInput(t.TempDir(), newSim(t, cfg))
			simulate(t, probe)
			bios, err := probe.Machine.Seams().Host.BIOSContext()
			if err != nil {
				t.Fatal(err)
			}
			var intent *journal.TrialIntent
			for _, e := range readEvents(t, probe.Dir) {
				if p, ok := e.Data.(*journal.TrialIntent); ok && p.Regime == machine.R7 && len(p.Cores) == 2 && p.Profile[0] != 0 && p.Profile[1] != 0 {
					intent = p
					break
				}
			}
			if intent == nil {
				t.Fatal("missing two-core R7 trial")
			}
			cfg.Replay, err = sim.NewReplay(bios, []sim.ReplayFact{{
				Context:   bios,
				Class:     journal.TrialClass{Regime: intent.Regime, Workload: intent.Workload, Cores: intent.Cores, DurationS: intent.DurationS},
				Profile:   intent.Profile,
				Outcome:   journal.OutcomeFailure,
				Signal:    machine.UncorrectedMCE,
				DurationS: 7,
			}})
			if err != nil {
				t.Fatal(err)
			}
			in := simInput(t.TempDir(), newSim(t, cfg))
			simulate(t, in)
			var end *journal.TrialEnd
			var failure *journal.Failure
			for _, e := range readEvents(t, in.Dir) {
				switch p := e.Data.(type) {
				case *journal.TrialEnd:
					if p.Trial == intent.Trial {
						end = p
					}
				case *journal.Failure:
					if p.Trial == intent.Trial {
						failure = p
					}
				}
			}
			if end == nil || end.Signal != machine.UncorrectedMCE || end.DurationS != 7 || end.Core != nil {
				t.Fatalf("shared-machine-check trial end %+v", end)
			}
			if failure == nil || failure.Signal != machine.UncorrectedMCE || failure.Attribution != tc.attribution {
				t.Fatalf("machine-check failure %+v, want %s", failure, tc.attribution)
			}
			if tc.local == 0 && failure.Core != nil || tc.local == 1 && (failure.Core == nil || *failure.Core != intent.Cores[0]) {
				t.Fatalf("machine-check core %+v, bank locality %g", failure, tc.local)
			}
		})
	}
}

func voltageSamples() []machine.TrialConditions {
	samples := []machine.TrialConditions{{ElapsedMS: 1000}, {ElapsedMS: 2000}, {ElapsedMS: 3000}, {ElapsedMS: 4000}}
	for i, value := range []float32{1.5, 1, 1.25} {
		table := &machine.PMTable{}
		table.VoltageRequestV[0], table.VoltageRequestV[1] = value, value+0.125
		table.VoltageRequestV[15] = 2
		samples[i].PMTable = table
	}
	return samples
}

type voltageTrials struct {
	machine.Trials
	samples []machine.TrialConditions
}

func (t voltageTrials) Samples(string) iter.Seq[machine.TrialConditions] {
	return slices.Values(t.samples)
}

func TestTrialRequestedVoltageAllRegimes(t *testing.T) {
	t.Parallel()
	for _, lanes := range []bool{true, false} {
		t.Run(fmt.Sprintf("lanes=%t", lanes), func(t *testing.T) {
			t.Parallel()
			in := simInput(t.TempDir(), newSim(t, small()))
			samples := voltageSamples()
			if !lanes {
				samples = []machine.TrialConditions{{ElapsedMS: 1000}}
			}
			seams := in.Machine.Seams()
			seams.Trials = voltageTrials{Trials: seams.Trials, samples: samples}
			if _, err := driveWithSeams(in, seams); err != nil {
				t.Fatal(err)
			}
			intents := make(map[string]*journal.TrialIntent)
			seen := make(map[machine.Regime]bool)
			for _, e := range readEvents(t, in.Dir) {
				switch p := e.Data.(type) {
				case *journal.TrialIntent:
					intents[p.Trial] = p
				case *journal.TrialEnd:
					intent := intents[p.Trial]
					seen[intent.Regime] = true
					var median, minimum *float64
					if lanes {
						shift := 0.0
						if intent.Core != nil && *intent.Core == 1 || slices.Contains(intent.Cores, 1) {
							shift = 0.125
						}
						median, minimum = new(1.25+shift), new(1.0+shift)
					}
					if diff := cmp.Diff([]*float64{median, minimum}, []*float64{p.VoltageRequestMedianV, p.VoltageRequestMinV}); diff != "" {
						t.Fatalf("%s %s voltage (-want +got):\n%s", intent.Regime, p.Trial, diff)
					}
					var raw map[string]json.RawMessage
					if err := json.Unmarshal(e.Raw, &raw); err != nil {
						t.Fatal(err)
					}
					for _, field := range []string{"voltage_request_median_v", "voltage_request_min_v"} {
						if diff := cmp.Diff(lanes, raw[field] != nil); diff != "" {
							t.Fatalf("%s presence (-want +got):\n%s", field, diff)
						}
					}
				}
			}
			want := map[machine.Regime]bool{machine.R1: true, machine.R2: true, machine.R3: true, machine.R4: true, machine.R5: true, machine.R6: true, machine.R7: true}
			if diff := cmp.Diff(want, seen); diff != "" {
				t.Fatalf("sampled regimes (-want +got):\n%s", diff)
			}
		})
	}
}

func TestCrashRecoveryRequestedVoltage(t *testing.T) {
	t.Parallel()
	in, _ := firstCrash(t, machine.ResetWatchdog, machine.Crash, false)
	dir := filepath.Join(in.Dir, "trials")
	trialDir := filepath.Join(dir, "0001")
	if err := os.MkdirAll(trialDir, 0755); err != nil {
		t.Fatal(err)
	}
	var contents []byte
	for _, sample := range voltageSamples() {
		line, err := json.Marshal(sample)
		if err != nil {
			t.Fatal(err)
		}
		contents = append(contents, append(line, '\n')...)
	}
	contents = append(contents, []byte(`{"elapsed_ms":5000,"pm_table":{"voltage_request_v":[2]}}`)...)
	if err := os.WriteFile(filepath.Join(trialDir, "samples.jsonl"), contents, 0644); err != nil {
		t.Fatal(err)
	}
	seams := in.Machine.Seams()
	seams.Trials = sampledTrials{Trials: seams.Trials, reader: trial.New(trial.Options{Dir: dir})}
	if _, err := driveWithSeams(in, seams); err != nil {
		t.Fatal(err)
	}
	for _, e := range readEvents(t, in.Dir) {
		if p, ok := e.Data.(*journal.TrialEnd); ok && p.Trial == "0001" {
			if diff := cmp.Diff([]*float64{new(1.25), new(1.0)}, []*float64{p.VoltageRequestMedianV, p.VoltageRequestMinV}); diff != "" {
				t.Fatalf("recovered voltage (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff(journal.OutcomeFailure, p.Outcome); diff != "" {
				t.Fatal(diff)
			}
			if diff := cmp.Diff("trial 0001 FAIL crash, last evidence 0s after start, last sample 4s | loaded voltage request median 1.250 V, min 1.000 V", e.Msg); diff != "" {
				t.Fatal(diff)
			}
			t.Log(string(e.Raw))
			return
		}
	}
	t.Fatal("missing recovered trial")
}

type crashDuringPartialTrial struct {
	Journal
	machine *sim.Machine
	signal  machine.Signal
	intent  *journal.TrialIntent
	named   int
}

func (j *crashDuringPartialTrial) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := j.Journal.Append(p, cause...)
	if err != nil {
		return e, err
	}
	if in, ok := p.(*journal.TrialIntent); ok && in.Regime == machine.R7 && len(in.Cores) >= 2 && len(in.Cores) < 4 && j.intent == nil {
		j.intent = in
	}
	if start, ok := p.(*journal.TrialStart); ok && j.intent != nil && start.Trial == j.intent.Trial {
		if j.signal != "" {
			// Name the deepest loaded core, which is not the partial's top requester.
			j.named = j.intent.Cores[0]
			for _, core := range j.intent.Cores {
				if j.intent.Profile[core] < j.intent.Profile[j.named] {
					j.named = core
				}
			}
			if _, err := j.Journal.Append(&journal.TrialProgress{Trial: start.Trial, Signal: j.signal, Core: new(j.named), Detail: "backend reported a computation error before the crash"}, e.Seq); err != nil {
				return e, err
			}
		}
		j.machine.Crash()
		return e, machine.ErrCrashed
	}
	return e, nil
}

type stopAfterPartialRecovery struct {
	Journal
	trial string
	ended bool
	next  *journal.TrialIntent
}

func (j *stopAfterPartialRecovery) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := j.Journal.Append(p, cause...)
	if err != nil {
		return e, err
	}
	if end, ok := p.(*journal.TrialEnd); ok && end.Trial == j.trial {
		j.ended = true
	}
	if in, ok := p.(*journal.TrialIntent); ok && j.ended {
		j.next = in
		return e, errKilled
	}
	return e, nil
}

func TestPartialCrashRecoversAndBacksOffWithoutHunt(t *testing.T) {
	t.Parallel()
	for _, signal := range []machine.Signal{"", machine.ComputationError} {
		t.Run(string(signal), func(t *testing.T) {
			t.Parallel()
			cfg := sim.Config{Seed: 4, Cores: 8, BIOS: make([]int, 8), Limits: make([]sim.Limits, 8)}
			for core := range cfg.Limits {
				for i := range cfg.Limits[core].Alone {
					cfg.Limits[core].Alone[i] = -50
				}
				for i := range cfg.Limits[core].Together {
					cfg.Limits[core].Together[i] = -50
				}
			}
			model := sim.DefaultModel()
			model.CrashMCE = 0
			cfg.Model = &model
			in := simInput(t.TempDir(), newSim(t, cfg))
			in.Config.CandidateSoloLimits = map[int]int{0:-20,1:-25,2:-30,3:-35,4:-20,5:-25,6:-30,7:-35}
			in.Config.Durations.ShortTrialS = 1
			in.Config.Durations.CheckingTrialS = 1
			in.Config.Durations.CheckingAllCoreS = 1
			in.Config.Evidence.Rate = .95
			in.Config.Evidence.Miss = .2
			in.Machine.NextReset(machine.ResetWatchdog)
			var interrupted *crashDuringPartialTrial
			_, err := simulateBoot(context.Background(), in, func(j *journal.Journal) Journal {
				interrupted = &crashDuringPartialTrial{Journal: wrapFor(in, nil)(j), machine: in.Machine, signal: signal}
				return interrupted
			})
			if !errors.Is(err, machine.ErrCrashed) || interrupted.intent == nil {
				t.Fatalf("partial crash: %v, %+v", err, interrupted)
			}
			partial := interrupted.intent
			in.Machine.Reboot()
			var resumed *stopAfterPartialRecovery
			_, err = simulateBoot(context.Background(), in, func(j *journal.Journal) Journal {
				resumed = &stopAfterPartialRecovery{Journal: wrapFor(in, nil)(j), trial: partial.Trial}
				return resumed
			})
			if !errors.Is(err, errKilled) || resumed.next == nil {
				t.Fatalf("resume: %v, %+v", err, resumed)
			}
			if diff := cmp.Diff(partial.Profile, resumed.next.Profile); diff == "" {
				t.Fatal("failed partial did not back off profile")
			}
			var end *journal.TrialEnd
			var failure *journal.Failure
			var move *journal.TunerDecision
			events := readEvents(t, in.Dir)
			for _, e := range events {
				switch p := e.Data.(type) {
				case *journal.TrialEnd:
					if p.Trial == partial.Trial {
						end = p
					}
				case *journal.Failure:
					if p.Trial == partial.Trial {
						failure = p
					}
				case *journal.TunerDecision:
					if p.Decision == journal.Backoff && p.Phase != journal.PhaseSearch {
						move = p
					}
				case *journal.HuntStart, *journal.Combination:
					t.Fatalf("R7 partial hunted: %+v", e)
				}
			}
			if end == nil || end.Outcome != journal.OutcomeFailure || failure == nil || move == nil {
				t.Fatalf("missing recovered failure/backoff: %+v %+v %+v", end, failure, move)
			}
			top := partial.Profile[partial.Cores[0]]
			for _, core := range partial.Cores {
				top = max(top, partial.Profile[core])
			}
			if signal != "" {
				named := interrupted.named
				if partial.Profile[named] >= top {
					t.Fatalf("fixture named core %d, a top requester of %v", named, partial.Profile)
				}
				if end.Signal != signal || end.Core == nil || *end.Core != named || failure.Attribution != journal.Attributed || failure.Core == nil || *failure.Core != named {
					t.Fatalf("recovery lost the named computation error: %+v %+v", end, failure)
				}
				if move.Core != named {
					t.Fatalf("named core %d backed off %d", named, move.Core)
				}
			} else {
				if end.Signal != machine.Crash || failure.Attribution != journal.Unattributed {
					t.Fatalf("recovered crash: %+v %+v", end, failure)
				}
				if !slices.Contains(partial.Cores, move.Core) || partial.Profile[move.Core] != top {
					t.Fatalf("unattributed crash backed off core %d, not a top requester of %v", move.Core, partial.Profile)
				}
			}
			for _, fact := range facts.FromEvents(events).Facts {
				if fact.Trial == partial.Trial && fact.RecordOnly {
					t.Fatal("partial remained record-only")
				}
			}
		})
	}
}
