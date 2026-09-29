package machine

import (
	"context"
	"errors"
	"time"
)

type Clock interface {
	Now() time.Time
	// Monotonic is CLOCK_MONOTONIC: the time since this boot started, unaffected by wall-clock jumps.
	Monotonic() time.Duration
	// Sleep returns ctx.Err() when ctx is cancelled first.
	Sleep(ctx context.Context, d time.Duration) error
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
	// Ranking returns the raw preferred-core ranking value of each core, in core-id order.
	Ranking() ([]int, error)
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
	// Signal reports a backend computation error the moment it is classified, before the trial ends.
	Signal(core int, signal Signal, detail string)
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
	Monotonic time.Duration
	Lines     []string
}

type ResetKind string

const (
	ResetWatchdog    ResetKind = "watchdog"
	ResetSyncFlood   ResetKind = "sync_flood"
	ResetCPUShutdown ResetKind = "cpu_shutdown"
	ResetPowerButton ResetKind = "power_button"
	ResetThermalTrip ResetKind = "thermal_trip"
	ResetPowerLoss   ResetKind = "power_loss"
	ResetUnknown     ResetKind = "unknown"
)

type ResetReason struct {
	// Kind is empty when the boot logged no reason line.
	Kind ResetKind
	Raw  string
	// Supported is true when the kernel is recent enough (6.16 or later) to log reset reasons.
	Supported bool
}

type Kernel interface {
	// MCEs returns the machine checks in the kernel log of boot `boot` at or after the boot-local monotonic time `since`; 0 is the whole boot.
	// Earlier boots come from the persistent system journal; an unknown boot has none.
	MCEs(boot string, since time.Duration) ([]MCE, error)
	// ResetReason reads what boot `boot`'s kernel logged about the reset before it.
	ResetReason(boot string) (ResetReason, error)
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

var ErrBackendMissing = errors.New("backend binary missing")

// ErrBootMissing means the system journal no longer holds the boot, for example after journald vacuumed it.
var ErrBootMissing = errors.New("boot missing from the system journal")
