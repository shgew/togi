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
	// ValidateSMU checks CPU family/model and driver codename without mailbox or SMN access.
	ValidateSMU() error
	BIOSContext() (BIOSContext, error)
	// Ranking returns the raw preferred-core ranking value of each core, in core-id order.
	Ranking() ([]int, error)
	// Preflight runs the common checks; BIOS context and tuning-boot watchdog readiness belong to the run loop.
	Preflight() []Check
	Watchdog() Check
}

// Check names: root, cpu, ryzen_smu, readback, slot_mapping, backends, systemd_run, watchdog.
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
	// Inconclusive records setup or required sampling loss; later valid samples cannot clear it.
	// Higher-precedence failure evidence can still decide the trial.
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
	// Signal reports a backend computation error, early exit or stall when classified, before the trial ends.
	Signal(core int, signal Signal, detail string)
}

type Running interface {
	Started() Started
	Wait(ctx context.Context, report Reporter) (Result, error)
}

// TrialConditions is an optional diagnostic sample, not stability evidence.
type TrialConditions struct {
	ElapsedMS     int64          `json:"elapsed_ms"`
	TctlC         *int           `json:"tctl_c,omitempty"`
	TccdC         map[string]int `json:"tccd_c,omitempty"`
	CoreMHz       map[int]int    `json:"core_mhz,omitempty"`
	PackagePowerW *float64       `json:"package_power_w,omitempty"`
}

type Trials interface {
	Sweep(ctx context.Context) (detail string, err error)
	Start(ctx context.Context, spec TrialSpec) (Running, error)
	// Passed marks a passed trial's work directory so retention may prune it.
	Passed(id string) error
	// LastSample returns the last complete persisted conditions sample, or nil.
	LastSample(id string) *TrialConditions
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

type KernelRead struct {
	MCEs   []MCE
	Cursor string
}

type Kernel interface {
	// MCEs returns the machine checks in the kernel log of boot `boot` at or after the boot-local monotonic time `since`; 0 is the whole boot.
	// Earlier boots come from the persistent system journal; an unknown boot has none.
	MCEs(boot string, since time.Duration) ([]MCE, error)
	// ReadMCEs reads after an exact boot-local cursor, or the whole boot when cursor is empty.
	// An unavailable cursor returns ErrCursorMissing, never a silently shortened interval.
	ReadMCEs(boot, cursor string) (KernelRead, error)
	// ResetReason reads what boot `boot`'s kernel logged about the reset before it.
	ResetReason(boot string) (ResetReason, error)
	// ResetReasonAfter reads the immediate system successor, including boots without togi.
	// An unidentified successor or an unproven journal boundary returns ErrBootMissing.
	ResetReasonAfter(boot string) (ResetReason, error)
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

var ErrContainment = errors.New("trial cleanup could not be confirmed")

// ErrBootMissing means the system journal no longer holds the boot, for example after journald vacuumed it.
var ErrBootMissing = errors.New("boot missing from the system journal")

var ErrCursorMissing = errors.New("kernel log cursor cannot be resumed")
