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

// failure is the first failure a trial meets. A set signal overrides the signal drawn for it, though the draw is
// still taken; a set joint replaces the core-local crash MCE draw.
type failure struct {
	core       int
	at         time.Duration
	signal     machine.Signal
	idle       bool
	background bool
	joint      *Joint
	reset      machine.ResetKind
	thenCrash  bool
	replayed   bool
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
	f, failed := m.trialFailure(spec)
	res := machine.Result{Ran: spec.Duration}
	if len(spec.Cores) > 0 {
		res.TctlMaxC = new(62 + m.trialRNG("tctl", spec, spec.Cores[0]).IntN(15))
	}
	if r.escape && len(spec.Cores) > 0 {
		res.Escaped = []int{spec.Cores[0] + 2*m.cfg.Cores}
	}
	if !failed {
		m.now = start.Add(spec.Duration)
		res = r.counted(res)
		r.progress(report, res)
		return res, nil
	}
	rng := m.trialRNG("signal", spec, f.core)
	signal := drawSignal(m.signals(spec.Regime), rng.Float64())
	if f.signal != "" {
		signal = f.signal
	}
	if f.thenCrash {
		stallCore = f.core
		m.now = start.Add(f.at)
		if report != nil && signal != machine.Crash {
			report.Signal(f.core, signal, fmt.Sprintf("simulated %s on core %d", signal, f.core))
		}
		if f.reset != "" {
			m.NextReset(f.reset)
		}
		m.Crash()
		return machine.Result{}, machine.ErrCrashed
	}
	switch signal {
	case machine.ComputationError, machine.Stall, machine.UnexpectedExit:
		m.now = start.Add(f.at)
		res.Ran, res.Signal, res.Core = f.at, signal, f.core
		res = r.counted(res)
		if signal == machine.ComputationError && report != nil {
			report.Signal(f.core, signal, fmt.Sprintf("simulated computation error on core %d", f.core))
		}
		r.progress(report, res)
		return res, nil
	case machine.CorrectedMCE:
		m.logMCE(m.bootID, m.mce(rng.Float64(), f.core, true), start.Add(f.at))
		if f.replayed {
			res.Ran = f.at
		}
		m.now = start.Add(res.Ran)
		res = r.counted(res)
		r.progress(report, res)
		return res, nil
	case machine.Crash:
		if !f.background {
			stallCore = f.core
		}
		m.now = start.Add(f.at)
		switch {
		case f.replayed:
			m.NextReset(machine.ResetWatchdog)
			if report != nil {
				report.Progress("simulated replayed crash at recorded exposure")
			}
		case f.reset != "":
			m.NextReset(f.reset)
		case m.nextReset == "":
			m.NextReset(m.drawReset(rng.Float64()))
		}
		r.progress(report, r.counted(machine.Result{Ran: f.at}))
		if !f.replayed {
			m.queueCrashMCE(rng, f.core, f.idle, f.joint)
		}
		m.Crash()
		return machine.Result{}, machine.ErrCrashed
	case machine.UncorrectedMCE:
		m.now = start.Add(f.at)
		if f.replayed && report != nil {
			report.Signal(f.core, machine.UncorrectedMCE, "simulated replayed uncorrected machine check")
		}
		m.queued = append(m.queued, m.mce(0, f.core, false))
		m.NextReset(machine.ResetSyncFlood)
		m.Crash()
		return machine.Result{}, machine.ErrCrashed
	}
	return machine.Result{}, fmt.Errorf("simulated trial: no signal to draw from %v", m.signals(spec.Regime))
}

// trialFailure answers a trial from a matching recorded fact, else from its script, else from the model's draws.
func (m *Machine) trialFailure(spec machine.TrialSpec) (failure, bool) {
	if fact, ok := m.replayDraw(spec); ok {
		return replayedFailure(fact)
	}
	if script, ok := m.cfg.Script[spec.ID]; ok {
		return scriptedFailure(script, spec)
	}
	return m.drawnFailure(spec)
}

func replayedFailure(fact ReplayFact) (failure, bool) {
	if fact.Outcome != journal.OutcomeFailure {
		return failure{}, false
	}
	return failure{core: fact.Class.Cores[0], at: time.Duration(fact.DurationS) * time.Second, signal: fact.Signal, replayed: true}, true
}

func scriptedFailure(script Outcome, spec machine.TrialSpec) (failure, bool) {
	if script.Signal == "" {
		return failure{}, false
	}
	at := time.Duration(script.AtS * float64(time.Second))
	return failure{
		core:      script.Core,
		at:        max(0, min(at, spec.Duration)),
		signal:    script.Signal,
		reset:     script.Reset,
		thenCrash: script.ThenCrash,
	}, true
}

// drawnFailure draws every hazard the trial is exposed to, in a fixed order, and keeps the earliest failure.
func (m *Machine) drawnFailure(spec machine.TrialSpec) (failure, bool) {
	f := failure{core: -1, at: spec.Duration}
	for c := range m.regs {
		if t, signal, ok := m.failureTime(spec, c); ok && t < f.at {
			f.core, f.at, f.signal = c, t, signal
			f.idle = !slices.Contains(spec.Cores, c)
		}
	}
	if m.sharedR7(spec) {
		state := m.voltageState(m.regs, spec)
		for core, rate := range state.rates {
			t := m.failureDraw(rate, 0, spec, core, "voltage")
			if t < f.at {
				f.core, f.at, f.signal = core, t, m.voltageSignal(spec, core)
				f.idle = false
				f.joint = &Joint{} // Shared-rail crashes never fabricate core-local MCEs.
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
		if t < f.at {
			f.core, f.at, f.signal = core, t, m.unattributedSignal()
			f.idle = false
			f.joint = &Joint{}
		}
	}
	for j, joint := range m.cfg.Joints {
		if m.sharedR7(spec) {
			break
		}
		rate := m.jointRate(m.regs, spec.Regime, j)
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
		if t < f.at {
			f.core, f.at, f.signal = core, t, joint.Signal
			f.idle = idle
			f.joint = &m.cfg.Joints[j]
			if idle {
				f.signal = machine.Crash
			} else if f.signal == "" {
				f.signal = m.unattributedSignal()
			}
		}
	}
	if m.sharedR7(spec) && m.cfg.SharedVoltage.BackgroundRate > 0 {
		core := spec.Cores[0]
		t := m.failureDraw(m.cfg.SharedVoltage.BackgroundRate, 0, spec, core, "voltage-background")
		if t < f.at {
			f.core, f.at, f.signal = core, t, machine.Crash
			f.idle = false
			f.background = true
			f.joint = &Joint{} // Platform background carries no core attribution.
		}
	}
	return f, f.core >= 0
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
	if slices.Contains(spec.Cores, core) {
		return m.rng(purpose, core, spec.Regime, condition, m.regs[core], spec.Index)
	}
	return m.rng(purpose, core, spec.Regime, condition, m.regs[core], spec.Index, slices.Sorted(slices.Values(spec.Cores)))
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

func regimeIndex(r machine.Regime) int {
	if len(r) == 2 && r[0] == 'R' && r[1] >= '1' && r[1] <= '7' {
		return int(r[1] - '1')
	}
	return -1
}

// limit is the deepest offset core passes in regime r; alone reports that every other core is at offset 0.
func (m *Machine) limit(core int, r machine.Regime, workload string, alone bool) int {
	for _, w := range m.workloadLimits[core] {
		if w.id == workload {
			return w.limit
		}
	}
	i := regimeIndex(r)
	if i < 0 {
		return 0
	}
	if alone && i < len(m.limits[core].Alone) {
		return m.limits[core].Alone[i]
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

// signals returns the weights failures in regime draw their signal from.
func (m *Machine) signals(regime machine.Regime) map[machine.Signal]float64 {
	if weights, ok := m.model.RegimeSignals[regime]; ok {
		return weights
	}
	return m.model.Signals
}

// unattributedSignal is the signal of a loaded joint or [ccd] failure without its own signal: a crash, unless
// the machine fits per-regime mixes, where the empty signal leaves it to the draw from m.signals.
func (m *Machine) unattributedSignal() machine.Signal {
	if len(m.model.RegimeSignals) > 0 {
		return ""
	}
	return machine.Crash
}

func drawSignal(weights map[machine.Signal]float64, x float64) machine.Signal {
	var total float64
	for _, s := range signalOrder {
		total += weights[s]
	}
	x *= total
	for _, s := range signalOrder {
		if w := weights[s]; w > 0 {
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
