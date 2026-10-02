package session

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"slices"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
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
			if stop.Reason != StopRotations {
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
	if stop := drive(t, in, crashAt(ref[index].Seq, in.Machine)); stop.Reason != StopRotations {
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

func runWithSeams(ctx context.Context, in simRun, seams machine.Machine) (Stop, error) {
	boot, err := seams.Host.BootID()
	if err != nil {
		return Stop{}, err
	}
	j, err := journal.Open(in.Dir, journal.Options{Boot: boot, Now: in.Machine.Now, Monotonic: seams.Clock.Monotonic, Build: Build()})
	if err != nil {
		return Stop{}, err
	}
	stop, err := Run(ctx, Input{Config: in.Config, ConfigPath: in.ConfigPath, Boot: boot, Journal: wrapFor(in, nil)(j), Machine: seams, Rotations: in.Rotations, Stderr: in.Stderr})
	if closeErr := j.Close(); err == nil {
		err = closeErr
	}
	return stop, err
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
			if err != nil || stop.Reason != StopRotations {
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

func TestInterruptedTrialDurationUsesMonotonicTimeAfterWallJump(t *testing.T) {
	t.Parallel()
	for _, jump := range []time.Duration{-time.Hour, time.Hour} {
		t.Run(jump.String(), func(t *testing.T) {
			t.Parallel()
			in := simInput(t.TempDir(), newSim(t, small()))
			seams := in.Machine.Seams()
			seams.Trials = &jumpAndCrashTrials{Trials: seams.Trials, m: in.Machine, jump: jump}
			stop, err := driveWithSeams(in, seams)
			if err != nil || stop.Reason != StopRotations {
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

func (t sampledTrials) LastSample(id string) *machine.TrialConditions {
	return t.reader.LastSample(id)
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

func TestReplaySharedMachineCheckKeepsBankAttribution(t *testing.T) {
	t.Parallel()
	cfg := small()
	model := sim.DefaultModel()
	model.CoreLocalBank, model.CrashMCE = 0, 0
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
	if failure == nil || failure.Signal != machine.UncorrectedMCE || failure.Attribution != journal.Unattributed || failure.Core != nil {
		t.Fatalf("shared-machine-check failure %+v", failure)
	}
}
