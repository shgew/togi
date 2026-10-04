package sim

import (
	"context"
	"errors"
	"fmt"
	"maps"
	"math/rand/v2"
	"slices"
	"strconv"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

var signalOrder = []machine.Signal{machine.ComputationError, machine.Stall, machine.UnexpectedExit, machine.CorrectedMCE, machine.Crash}

var (
	bankTypes = []machine.BankType{
		machine.LoadStore, machine.InstructionFetch, machine.L2Cache, machine.DecodeUnit, machine.ExecutionUnit,
		machine.FloatingPoint, machine.L3Cache, machine.MemoryController, machine.DataFabric,
	}
	coreLocalBanks = bankTypes[:6]
	sharedBanks    = bankTypes[6:]
)

type trials struct{ m *Machine }

type running struct {
	m       *Machine
	boot    int
	spec    machine.TrialSpec
	started machine.Started
	escape  bool
	stopped bool
	stopErr error
}

func (trials) Passed(string) error { return nil }

func (trials) Sweep(ctx context.Context) (string, error) {
	return "simulated machine has no leftover trial scopes", ctx.Err()
}

func (t trials) Start(ctx context.Context, spec machine.TrialSpec) (machine.Running, error) {
	m := t.m
	if m.crashed {
		return nil, machine.ErrCrashed
	}
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if m.failSetup > 0 {
		m.failSetup--
		return nil, errors.New("simulated backend setup failure")
	}
	if m.missingBackends[spec.Workload.Backend] {
		return nil, fmt.Errorf("start simulated backend %s: %w", spec.Workload.Backend, machine.ErrBackendMissing)
	}
	if spec.Condition == machine.Alone {
		for c, o := range m.regs {
			if o != 0 && !slices.Contains(spec.Cores, c) {
				m.violations = append(m.violations, fmt.Sprintf("trial %s: core %d at %d during a trial alone on cores %v", spec.ID, c, o, spec.Cores))
			}
		}
	}
	n, _ := strconv.Atoi(spec.ID)
	r := &running{
		m:    m,
		boot: m.boot,
		spec: spec,
		started: machine.Started{
			PID:      1000 + n,
			Scope:    "togi-trial-" + spec.ID,
			CPUs:     slices.Clone(spec.CPUs),
			Argv:     []string{"sim", spec.Workload.ID},
			Schedule: machine.ScheduleFor(spec),
		},
		escape: m.escape,
	}
	m.escape = false
	return r, nil
}

func (r *running) Started() machine.Started { return r.started }

func (r *running) Stop() error {
	if !r.stopped {
		r.stopped = true
		if r.m.crashed || r.boot != r.m.boot {
			r.stopErr = machine.ErrCrashed
		}
	}
	return r.stopErr
}

func (r *running) Wait(ctx context.Context, report machine.Reporter) (result machine.Result, err error) {
	m := r.m
	if m.crashed || r.boot != m.boot {
		return machine.Result{}, machine.ErrCrashed
	}
	if err := ctx.Err(); err != nil {
		return machine.Result{}, err
	}
	spec := r.spec
	start := m.now
	stallCore := -1
	defer func() { err = errors.Join(err, r.sampleConditions(m.now.Sub(start), stallCore)) }()
	failCore, failAt, forcedSignal := -1, spec.Duration, machine.Signal("")
	idleFailure := false
	backgroundFailure := false
	var jointCrash *Joint
	script, scripted := m.cfg.Script[spec.ID]
	fact, replayed := m.replayDraw(spec)
	switch {
	case replayed:
		scripted = false
		if fact.Outcome == journal.OutcomeFailure {
			failCore = fact.Class.Cores[0]
			failAt = time.Duration(fact.DurationS) * time.Second
			forcedSignal = fact.Signal
		}
	case scripted:
		if script.Signal != "" {
			failCore, failAt = script.Core, time.Duration(script.AtS*float64(time.Second))
			failAt = max(0, min(failAt, spec.Duration))
		}
	default:
		for c := range m.regs {
			if t, signal, ok := m.failureTime(spec, c); ok && t < failAt {
				failCore, failAt, forcedSignal = c, t, signal
				idleFailure = !slices.Contains(spec.Cores, c)
			}
		}
		if m.sharedR7(spec) {
			state := m.voltageState(m.regs, spec)
			for core, rate := range state.rates {
				t := m.failureDraw(rate, 0, spec, core, "voltage")
				if t < failAt {
					failCore, failAt, forcedSignal = core, t, m.voltageSignal(spec, core)
					idleFailure = false
					jointCrash = &Joint{} // Shared-rail crashes never fabricate core-local MCEs.
				}
			}
		}
		for ccd := range 2 {
			rate := m.ccdRate(m.regs, spec, ccd)
			if rate <= 0 {
				continue
			}
			core := -1
			for _, loaded := range spec.Cores {
				if loaded/(m.cfg.Cores/2) == ccd {
					core = loaded
					break
				}
			}
			t := m.failureDraw(rate, 0, spec, core, fmt.Sprintf("ccd-%d", ccd))
			if t < failAt {
				failCore, failAt, forcedSignal = core, t, machine.Crash
				idleFailure = false
				jointCrash = &Joint{}
			}
		}
		for j, joint := range m.cfg.Joints {
			if m.sharedR7(spec) {
				break
			}
			rate := m.jointRate(m.regs, spec.Regime, joint)
			if rate <= 0 {
				continue
			}
			core := -1
			for _, c := range spec.Cores {
				if _, ok := joint.Members[c]; ok {
					core = c
					break
				}
			}
			idle := core < 0
			if idle {
				core = slices.Min(slices.Collect(maps.Keys(joint.Members)))
			}
			t := m.failureDraw(rate, joint.AfterS, spec, core, fmt.Sprintf("joint-%d", j))
			if t < failAt {
				failCore, failAt, forcedSignal = core, t, joint.Signal
				idleFailure = idle
				jointCrash = &m.cfg.Joints[j]
				if idle || forcedSignal == "" {
					forcedSignal = machine.Crash
				}
			}
		}
		if m.sharedR7(spec) && m.cfg.SharedVoltage.BackgroundRate > 0 {
			core := spec.Cores[0]
			t := m.failureDraw(m.cfg.SharedVoltage.BackgroundRate, 0, spec, core, "voltage-background")
			if t < failAt {
				failCore, failAt, forcedSignal = core, t, machine.Crash
				idleFailure = false
				backgroundFailure = true
				jointCrash = &Joint{} // Platform background carries no core attribution.
			}
		}
	}
	res := machine.Result{Ran: spec.Duration}
	if len(spec.Cores) > 0 {
		res.TctlMaxC = new(62 + m.trialRNG("tctl", spec, spec.Cores[0]).IntN(15))
	}
	if r.escape && len(spec.Cores) > 0 {
		res.Escaped = []int{spec.Cores[0] + 2*m.cfg.Cores}
	}
	if failCore < 0 {
		m.now = start.Add(spec.Duration)
		res = r.counted(res)
		r.progress(report, res)
		return res, nil
	}
	rng := m.trialRNG("signal", spec, failCore)
	signal := m.drawSignal(rng.Float64())
	if forcedSignal != "" {
		signal = forcedSignal
	}
	if scripted {
		signal = script.Signal
	}
	if scripted && script.ThenCrash {
		stallCore = failCore
		m.now = start.Add(failAt)
		if report != nil && signal != machine.Crash {
			report.Signal(failCore, signal, fmt.Sprintf("simulated %s on core %d", signal, failCore))
		}
		if script.Reset != "" {
			m.NextReset(script.Reset)
		}
		m.Crash()
		return machine.Result{}, machine.ErrCrashed
	}
	switch signal {
	case machine.ComputationError, machine.Stall, machine.UnexpectedExit:
		m.now = start.Add(failAt)
		res.Ran, res.Signal, res.Core = failAt, signal, failCore
		res = r.counted(res)
		if signal == machine.ComputationError && report != nil {
			report.Signal(failCore, signal, fmt.Sprintf("simulated computation error on core %d", failCore))
		}
		r.progress(report, res)
		return res, nil
	case machine.CorrectedMCE:
		m.logMCE(m.bootID, m.mce(rng.Float64(), failCore, true), start.Add(failAt))
		if replayed {
			res.Ran = failAt
		}
		m.now = start.Add(res.Ran)
		res = r.counted(res)
		r.progress(report, res)
		return res, nil
	case machine.Crash:
		if !backgroundFailure {
			stallCore = failCore
		}
		m.now = start.Add(failAt)
		switch {
		case replayed:
			m.NextReset(machine.ResetWatchdog)
			if report != nil {
				report.Progress("simulated replayed crash at recorded exposure")
			}
		case scripted && script.Reset != "":
			m.NextReset(script.Reset)
		case m.nextReset == "":
			m.NextReset(m.drawReset(rng.Float64()))
		}
		r.progress(report, r.counted(machine.Result{Ran: failAt}))
		if !replayed {
			m.queueCrashMCE(rng, failCore, idleFailure, jointCrash)
		}
		m.Crash()
		return machine.Result{}, machine.ErrCrashed
	case machine.UncorrectedMCE:
		m.now = start.Add(failAt)
		if replayed && report != nil {
			report.Signal(failCore, machine.UncorrectedMCE, "simulated replayed uncorrected machine check")
		}
		m.queued = append(m.queued, m.mce(0, failCore, false))
		m.NextReset(machine.ResetSyncFlood)
		m.Crash()
		return machine.Result{}, machine.ErrCrashed
	}
	return machine.Result{}, fmt.Errorf("simulated trial: no signal to draw from %v", m.model.Signals)
}

func (m *Machine) queueCrashMCE(rng *rand.Rand, core int, idle bool, joint *Joint) {
	if joint != nil {
		if joint.CrashMCECore != nil {
			m.queued = append(m.queued, machine.MCE{CPU: *joint.CrashMCECore, Core: *joint.CrashMCECore, Bank: 0, BankType: machine.LoadStore})
		}
		return
	}
	if !idle && rng.Float64() < m.model.CrashMCE {
		m.queued = append(m.queued, m.mce(rng.Float64(), core, false))
	}
}

func (r *running) counted(res machine.Result) machine.Result {
	if s := r.started.Schedule; s != nil {
		res.Stops, res.Conts = s.Counts(res.Ran)
	}
	if r.spec.Regime == machine.R6 && res.Ran > 0 {
		res.Stops = len(r.spec.Cores)
		for at := r.spec.Duration / 2; at < res.Ran && at < r.spec.Duration; at += 2 * time.Second {
			res.Conts++
			if at+100*time.Millisecond < res.Ran && at+100*time.Millisecond < r.spec.Duration {
				res.Stops++
			}
		}
	}
	return res
}

func (r *running) progress(report machine.Reporter, res machine.Result) {
	if report == nil {
		return
	}
	switch r.spec.Regime {
	case machine.R6:
		if res.Ran > r.spec.Duration/2 {
			report.Progress("first half idle, then 100ms bursts every 2s, one core at a time")
		}
		report.Progress(fmt.Sprintf("bursts: %d continues, %d stops", res.Conts, res.Stops))
	case machine.R1, machine.R2, machine.R3, machine.R4, machine.R5, machine.R7:
	}
}

func (m *Machine) trialRNG(purpose string, spec machine.TrialSpec, core int) *rand.Rand {
	// Seed domains are immutable even when condition wire names change.
	condition := string(spec.Condition)
	switch spec.Condition {
	case machine.Alone:
		condition = "isolated"
	case machine.Together:
		condition = "resident"
	case machine.Parked:
		condition = "masked"
	}
	return m.rng(purpose, core, spec.Regime, condition, m.regs[core], spec.Index)
}

func (m *Machine) failureTime(spec machine.TrialSpec, core int) (time.Duration, machine.Signal, bool) {
	loaded := slices.Contains(spec.Cores, core)
	rate := m.coreRate(m.regs, spec, core)
	t := m.failureDraw(rate, 0, spec, core, "trial")
	if t >= spec.Duration {
		return 0, "", false
	}
	if !loaded {
		return t, machine.Crash, true
	}
	return t, "", true
}

func (m *Machine) failureDraw(rate, afterS float64, spec machine.TrialSpec, core int, purpose string) time.Duration {
	if rate <= 0 {
		return spec.Duration
	}
	hazard := m.trialRNG(purpose, spec, core).ExpFloat64()
	onset := m.model.OnsetS
	seconds := hazard / rate
	if onset > afterS && m.model.OnsetBoost > 0 {
		early := onset - afterS
		boost := 1 + m.model.OnsetBoost
		seconds = hazard / (rate * boost)
		if seconds > early {
			seconds = early + (hazard-early*rate*boost)/rate
		}
	}
	if seconds+afterS >= spec.Duration.Seconds() {
		return spec.Duration
	}
	return time.Duration((afterS + seconds) * float64(time.Second))
}

func (m *Machine) limit(profile []int, core int, r machine.Regime, workload string) int {
	if limit, ok := m.limits[core].Workload[workload]; ok {
		return limit
	}
	i := slices.Index(machine.Regimes, r)
	if i < 0 {
		return 0
	}
	if i < len(m.limits[core].Alone) {
		only := true
		for c, offset := range profile {
			if c != core && offset != 0 {
				only = false
				break
			}
		}
		if only {
			return m.limits[core].Alone[i]
		}
	}
	return m.limits[core].Together[i]
}

var resetOrder = []machine.ResetKind{machine.ResetWatchdog, machine.ResetSyncFlood, machine.ResetCPUShutdown, machine.ResetPowerButton, machine.ResetThermalTrip, machine.ResetPowerLoss, machine.ResetUnknown}

func (m *Machine) drawReset(x float64) machine.ResetKind {
	var total float64
	for _, kind := range resetOrder {
		total += m.model.Reset[kind]
	}
	if total == 0 {
		return machine.ResetWatchdog
	}
	x *= total
	for _, kind := range resetOrder {
		x -= m.model.Reset[kind]
		if x < 0 {
			return kind
		}
	}
	return machine.ResetWatchdog
}

func (m *Machine) drawSignal(x float64) machine.Signal {
	var total float64
	for _, s := range signalOrder {
		total += m.model.Signals[s]
	}
	x *= total
	for _, s := range signalOrder {
		if w := m.model.Signals[s]; w > 0 {
			if x < w {
				return s
			}
			x -= w
		}
	}
	return machine.UncorrectedMCE
}

func (m *Machine) mce(x float64, core int, corrected bool) machine.MCE {
	banks := sharedBanks
	if x < m.model.CoreLocalBank {
		banks = coreLocalBanks
		x /= m.model.CoreLocalBank
	} else {
		x = (x - m.model.CoreLocalBank) / (1 - m.model.CoreLocalBank)
	}
	bt := banks[min(int(x*float64(len(banks))), len(banks)-1)]
	return machine.MCE{CPU: core, Core: core, Bank: slices.Index(bankTypes, bt), BankType: bt, Corrected: corrected}
}

func (m *Machine) logMCE(boot string, mce machine.MCE, at time.Time) {
	kind := "Uncorrected"
	if mce.Corrected {
		kind = "Corrected"
	}
	mce.Monotonic = at.Sub(m.bootAt)
	mce.Time = at.Add(m.wallOffset)
	mce.Lines = []string{fmt.Sprintf("[Hardware Error]: %s error on CPU %d bank %d (%s) at %s (simulated)", kind, mce.CPU, mce.Bank, mce.BankType, at.Format(time.RFC3339Nano))}
	m.logs[boot] = append(m.logs[boot], mce)
}
