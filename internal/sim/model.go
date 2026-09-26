package sim

import (
	"context"
	"errors"
	"fmt"
	"math"
	"math/rand/v2"
	"slices"
	"strconv"
	"time"

	"github.com/shgew/shycler/internal/machine"
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
}

func (trials) Passed(string) error { return nil }

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
	if spec.Condition == machine.Isolated {
		for c, o := range m.regs {
			if o != 0 && !slices.Contains(spec.Cores, c) {
				m.violations = append(m.violations, fmt.Sprintf("trial %s: core %d at %d during an isolated trial on cores %v", spec.ID, c, o, spec.Cores))
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
			Scope:    "shycler-trial-" + spec.ID,
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

func (r *running) Wait(ctx context.Context, report machine.Reporter) (machine.Result, error) {
	m := r.m
	if m.crashed || r.boot != m.boot {
		return machine.Result{}, machine.ErrCrashed
	}
	if err := ctx.Err(); err != nil {
		return machine.Result{}, err
	}
	spec := r.spec
	start := m.now
	failCore, failAt := -1, spec.Duration
	for _, c := range spec.Cores {
		if t, ok := m.failureTime(spec, c); ok && t < failAt {
			failCore, failAt = c, t
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
	switch signal {
	case machine.ComputationError, machine.Stall, machine.UnexpectedExit:
		m.now = start.Add(failAt)
		res.Ran, res.Signal, res.Core = failAt, signal, failCore
		res = r.counted(res)
		r.progress(report, res)
		return res, nil
	case machine.CorrectedMCE:
		m.logMCE(m.bootID, m.mce(rng.Float64(), failCore, true), start.Add(failAt))
		m.now = start.Add(spec.Duration)
		res = r.counted(res)
		r.progress(report, res)
		return res, nil
	case machine.Crash:
		m.now = start.Add(failAt)
		r.progress(report, r.counted(machine.Result{Ran: failAt}))
		if rng.Float64() < m.model.CrashMCE {
			m.queued = append(m.queued, m.mce(rng.Float64(), failCore, false))
		}
		m.Crash()
		return machine.Result{}, machine.ErrCrashed
	case machine.UncorrectedMCE:
	}
	return machine.Result{}, fmt.Errorf("simulated trial: no signal to draw from %v", m.model.Signals)
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
	return m.rng(purpose, core, spec.Regime, spec.Condition, m.regs[core], spec.Index)
}

func (m *Machine) failureTime(spec machine.TrialSpec, core int) (time.Duration, bool) {
	edge := m.edge(core, spec.Regime, spec.Condition)
	d := edge - m.regs[core]
	rate := m.model.NearEdgeRate
	if d >= 1 {
		rate = m.model.PastEdgeRate * math.Pow(m.model.Growth, float64(d-1))
	}
	if rate <= 0 {
		return 0, false
	}
	seconds := m.trialRNG("trial", spec, core).ExpFloat64() / rate
	if seconds >= spec.Duration.Seconds() {
		return 0, false
	}
	return time.Duration(seconds * float64(time.Second)), true
}

func (m *Machine) edge(core int, r machine.Regime, cond machine.Condition) int {
	i := slices.Index(machine.Regimes, r)
	switch cond {
	case machine.Isolated:
		if i < len(m.edges[core].Isolated) {
			return m.edges[core].Isolated[i]
		}
	case machine.Resident:
	}
	return m.edges[core].Resident[i]
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
	mce.Time = at
	mce.Lines = []string{fmt.Sprintf("[Hardware Error]: %s error on CPU %d bank %d (%s) at %s (simulated)", kind, mce.CPU, mce.Bank, mce.BankType, at.Format(time.RFC3339Nano))}
	m.logs[boot] = append(m.logs[boot], mce)
}
