package machine

import (
	"context"
	"errors"
	"time"
)

type Clock interface {
	Now() time.Time
}

type SMU interface {
	Offset(core int) (int, error)
	SetOffset(core, offset int) error
	SetAllOffsets(offset int) error
}

type Host interface {
	BootID() (string, error)
	Topology() ([]CoreInfo, error)
	BIOSContext() (BIOSContext, error)
	// Preflight runs checks 1-7 of runtime.md; check 8, the BIOS context, belongs to the run loop.
	Preflight() []Check
}

// Check names: root, cpu, ryzen_smu, readback, slot_mapping, backends, systemd_run.
type Check struct {
	Name   string
	Detail string
	OK     bool
}

type TrialSpec struct {
	ID        string
	Regime    Regime
	Workload  Workload
	Condition Condition
	Cores     []int
	CPUs      []int
	Duration  time.Duration
	// Index counts the conclusive trials of this regime on this core so far; it picks the workload unless the tuner chose one, and seeds the simulator.
	Index int
	// Seed is the R3 load-step seed; the run loop passes the trial.intent seq, which is never 0.
	Seed uint64
}

type Started struct {
	PID   int
	Scope string
	CPUs  []int
	Argv  []string
	// Files are the config files written into the trial directory, relative to it.
	Files     []string
	Instances []Instance
	// Schedule is the SIGSTOP/SIGCONT plan the runner follows; nil outside R3 and R4.
	Schedule *LoadSchedule
}

type Result struct {
	Ran time.Duration
	// Signal is empty or one of ComputationError, UnexpectedExit, Stall.
	Signal Signal
	// Core is the core of the instance that produced Signal.
	Core int
	// Inconclusive, when not empty, says why the trial proves nothing.
	Inconclusive string
	// Escaped lists logical CPUs a backend thread was seen on outside the trial's CPUs.
	Escaped []int
	// TctlMaxC is nil when there is no Tctl sensor.
	TctlMaxC *int
	Stops    int
	Conts    int
}

type Instance struct {
	Core  int
	CPUs  []int
	PID   int
	Scope string
}

type Sample struct {
	Warning string
	PID     int
	TID     int
	CPU     int
}

// Reporter is called only on the goroutine that called Wait.
type Reporter interface {
	Progress(detail string)
	Sample(s Sample)
}

type Running interface {
	Started() Started
	Wait(ctx context.Context, report Reporter) (Result, error)
}

type Trials interface {
	Start(ctx context.Context, spec TrialSpec) (Running, error)
	// Passed marks a passed trial's work directory so retention may prune it.
	Passed(id string) error
}

type MCE struct {
	CPU       int
	Core      int
	Bank      int
	BankType  BankType
	Corrected bool
	Time      time.Time
	Lines     []string
}

type Kernel interface {
	// MCEs returns the machine checks in the kernel log of boot `boot` at or after `since`.
	// Earlier boots come from the persistent system journal; an unknown boot has none.
	MCEs(boot string, since time.Time) ([]MCE, error)
}

type Machine struct {
	Clock  Clock
	SMU    SMU
	Host   Host
	Trials Trials
	Kernel Kernel
}

// ErrCrashed is returned only by simulated seams.
var ErrCrashed = errors.New("the simulated machine crashed")
