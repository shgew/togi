package sim

import (
	"context"
	"errors"
	"fmt"
	"hash/fnv"
	"maps"
	"math"
	"math/rand/v2"
	"slices"
	"strings"
	"time"

	"github.com/shgew/togi/internal/machine"
)

type Config struct {
	Seed uint64
	// Cores defaults to 16; it must be even and at least 2. Core c is on CCD c/(Cores/2) with CPUs c and c+Cores.
	Cores int
	// BIOS holds the offsets restored at every boot; default all 0.
	BIOS        []int
	BIOSContext machine.BIOSContext
	// Edges are the hidden edges; nil draws them from Seed.
	Edges []Edges
	// Model nil means DefaultModel(); a non-nil model is used verbatim, zero fields included.
	Model *Model
	// Boots counts the boots before the first; boot numbering and boot IDs continue from it.
	Boots int
	// Start is the clock at the first boot; zero means 2026-01-01T00:00:00Z.
	Start     time.Time
	Joints    []Joint
	Ranking   []int
	Script    map[string]Outcome
	OldKernel bool
}

// Edges index 0 is R1; Resident[5] and Resident[6] are R6 and R7.
type Edges struct {
	Isolated [5]int
	Resident [7]int
	Idle     *int
	Workload map[string]int
	Flat     float64
}

type Joint struct {
	Members      map[int]int
	Regimes      []machine.Regime
	Rate         float64
	AfterS       float64
	Signal       machine.Signal
	CrashMCECore *int
}

type Outcome struct {
	Signal    machine.Signal
	AtS       float64
	Core      int
	Reset     machine.ResetKind
	ThenCrash bool
}

type Model struct {
	PastEdgeRate  float64
	Growth        float64
	NearEdgeRate  float64
	Signals       map[machine.Signal]float64
	CrashMCE      float64
	CoreLocalBank float64
	OnsetS        float64
	OnsetBoost    float64
	Reset         map[machine.ResetKind]float64
}

func DefaultModel() Model {
	return Model{
		PastEdgeRate: math.Ln10 / 90,
		Growth:       4,
		NearEdgeRate: 0,
		Signals: map[machine.Signal]float64{
			machine.ComputationError: 4,
			machine.Stall:            1,
			machine.UnexpectedExit:   1,
			machine.CorrectedMCE:     2,
			machine.Crash:            2,
		},
		CrashMCE:      0.5,
		CoreLocalBank: 0.8,
		OnsetS:        100,
		Reset:         map[machine.ResetKind]float64{machine.ResetWatchdog: 1},
	}
}

var defaultBIOSContext = machine.BIOSContext{
	BIOSVersion:   "SIM.1",
	Board:         "togi simulator",
	CPUModel:      "Simulated Zen 5 16-Core Processor",
	Microcode:     "0x0",
	BoostLimitMHz: 5700,
}

var epoch = time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)

const RebootTime = 90 * time.Second

type Machine struct {
	cfg        Config
	model      Model
	edges      []Edges
	regs       []int
	boot       int
	bootID     string
	now        time.Time
	bootAt     time.Time
	wallOffset time.Duration
	crashed    bool
	logs       map[string][]machine.MCE
	queued     []machine.MCE
	violations []string
	bios       machine.BIOSContext
	reasons    map[string]machine.ResetReason
	nextReset  machine.ResetKind

	failWrite        bool
	failWriteAt      int
	crashAtWrite     int
	missingBackends  map[machine.Backend]bool
	corruptArmed     map[int]bool
	corruptPending   map[int]bool
	failSetup        int
	escape           bool
	failedChecks     map[string]string
	crashBeforeApply int
	wroteThisBoot    bool
}

func validateCores(cores int) error {
	if cores < 2 || cores%2 != 0 {
		return fmt.Errorf("%d cores: must be even and at least 2", cores)
	}
	return nil
}

func New(cfg Config) (*Machine, error) {
	if cfg.Cores == 0 {
		cfg.Cores = 16
	}
	if err := validateCores(cfg.Cores); err != nil {
		return nil, fmt.Errorf("new simulator: %w", err)
	}
	if cfg.BIOS == nil {
		cfg.BIOS = make([]int, cfg.Cores)
	}
	if len(cfg.BIOS) != cfg.Cores {
		return nil, fmt.Errorf("new simulator: %d BIOS offsets for %d cores", len(cfg.BIOS), cfg.Cores)
	}
	for c, o := range cfg.BIOS {
		if o < machine.MinOffset || o > machine.MaxOffset {
			return nil, fmt.Errorf("new simulator: BIOS offset %d of core %d outside [-50, 0]", o, c)
		}
	}
	if cfg.BIOSContext == (machine.BIOSContext{}) {
		cfg.BIOSContext = defaultBIOSContext
	}
	model := DefaultModel()
	if cfg.Model != nil {
		model = *cfg.Model
	}
	start := epoch
	if !cfg.Start.IsZero() {
		start = cfg.Start
	}
	for _, signal := range slices.Sorted(maps.Keys(model.Signals)) {
		weight := model.Signals[signal]
		if !slices.Contains(signalOrder, signal) {
			return nil, fmt.Errorf("new simulator: signal %q is not supported", signal)
		}
		if weight < 0 {
			return nil, fmt.Errorf("new simulator: signal %q weight %g is negative", signal, weight)
		}
	}
	for _, kind := range slices.Sorted(maps.Keys(model.Reset)) {
		weight := model.Reset[kind]
		if !slices.Contains(resetOrder, kind) {
			return nil, fmt.Errorf("new simulator: reset %q is not supported", kind)
		}
		if weight < 0 {
			return nil, fmt.Errorf("new simulator: reset %q weight %g is negative", kind, weight)
		}
	}
	for _, trial := range slices.Sorted(maps.Keys(cfg.Script)) {
		s := cfg.Script[trial]
		if s.Core < 0 || s.Core >= cfg.Cores {
			return nil, fmt.Errorf("new simulator: script trial %s core %d outside [0, %d)", trial, s.Core, cfg.Cores)
		}
		if s.Signal != "" && !slices.Contains(signalOrder, s.Signal) {
			return nil, fmt.Errorf("new simulator: script trial %s signal %q is not supported", trial, s.Signal)
		}
		if s.Reset != "" && !slices.Contains(resetOrder, s.Reset) {
			return nil, fmt.Errorf("new simulator: script trial %s reset %q is not supported", trial, s.Reset)
		}
	}
	m := &Machine{
		cfg:             cfg,
		model:           model,
		edges:           cfg.Edges,
		boot:            cfg.Boots,
		now:             start,
		logs:            map[string][]machine.MCE{},
		bios:            cfg.BIOSContext,
		corruptArmed:    map[int]bool{},
		corruptPending:  map[int]bool{},
		reasons:         map[string]machine.ResetReason{},
		missingBackends: map[machine.Backend]bool{},
		failedChecks:    map[string]string{},
	}
	if m.edges == nil {
		m.edges = m.drawEdges()
	}
	if len(m.edges) != cfg.Cores {
		return nil, fmt.Errorf("new simulator: %d edges for %d cores", len(m.edges), cfg.Cores)
	}
	for c, e := range m.edges {
		for _, v := range slices.Concat(e.Isolated[:], e.Resident[:]) {
			if v < machine.MinOffset || v > 1 {
				return nil, fmt.Errorf("new simulator: edge %d of core %d outside [-50, 1]", v, c)
			}
		}
	}
	if cfg.Ranking != nil && len(cfg.Ranking) != cfg.Cores {
		return nil, fmt.Errorf("new simulator: %d ranking values for %d cores", len(cfg.Ranking), cfg.Cores)
	}
	for c, e := range m.edges {
		if e.Idle != nil && (*e.Idle < machine.MinOffset || *e.Idle > 1) {
			return nil, fmt.Errorf("new simulator: idle edge %d of core %d outside [-50, 1]", *e.Idle, c)
		}
		for workload, offset := range e.Workload {
			if offset < machine.MinOffset || offset > 1 {
				return nil, fmt.Errorf("new simulator: workload %s edge %d of core %d outside [-50, 1]", workload, offset, c)
			}
		}
		if e.Flat < 0 {
			return nil, fmt.Errorf("new simulator: flat rate %g of core %d is negative", e.Flat, c)
		}
	}
	if err := validateJoints(cfg.Joints, cfg.Cores); err != nil {
		return nil, fmt.Errorf("new simulator: %w", err)
	}
	m.startBoot()
	return m, nil
}

func validateJoints(joints []Joint, cores int) error {
	for _, joint := range joints {
		for c, offset := range joint.Members {
			if c < 0 || c >= cores || offset < machine.MinOffset || offset > machine.MaxOffset {
				return fmt.Errorf("joint member core %d offset %d invalid", c, offset)
			}
		}
		for _, r := range joint.Regimes {
			if !slices.Contains(machine.Regimes, r) {
				return fmt.Errorf("joint regime %q is not supported", r)
			}
		}
		if joint.Signal != "" && !slices.Contains(signalOrder, joint.Signal) {
			return fmt.Errorf("joint signal %q is not supported", joint.Signal)
		}
		if joint.Rate < 0 || joint.AfterS < 0 {
			return fmt.Errorf("joint rate %g or delay %g is negative", joint.Rate, joint.AfterS)
		}
		if core := joint.CrashMCECore; core != nil && (*core < 0 || *core >= cores) {
			return fmt.Errorf("joint crash MCE core %d outside [0, %d)", *core, cores)
		}
	}
	return nil
}

func (m *Machine) drawEdges() []Edges {
	r := m.rng("edges")
	resident := func() int {
		if r.Float64() < 0.75 {
			return 0
		}
		return 1 + r.IntN(3)
	}
	edges := make([]Edges, m.cfg.Cores)
	for c := range edges {
		base := -5 - r.IntN(36)
		deepest := machine.MinOffset
		for i := range edges[c].Isolated {
			edges[c].Isolated[i] = machine.ClampOffset(base + r.IntN(3))
			deepest = max(deepest, edges[c].Isolated[i])
		}
		for i := range 5 {
			edges[c].Resident[i] = machine.ClampOffset(edges[c].Isolated[i] + resident())
		}
		edges[c].Resident[5] = machine.ClampOffset(deepest + resident())
		edges[c].Resident[6] = machine.ClampOffset(deepest + resident())
	}
	return edges
}

func fnv64(parts ...any) uint64 {
	h := fnv.New64a()
	s := make([]string, len(parts))
	for i, p := range parts {
		s[i] = fmt.Sprintf("%v", p)
	}
	h.Write([]byte(strings.Join(s, "|")))
	return h.Sum64()
}

func (m *Machine) rng(parts ...any) *rand.Rand {
	return rand.New(rand.NewPCG(m.cfg.Seed, fnv64(append([]any{m.cfg.Seed}, parts...)...)))
}

func (m *Machine) startBoot() {
	m.boot++
	m.bootAt = m.now
	r := m.rng("boot", m.boot)
	hex := fmt.Sprintf("%016x%016x", r.Uint64(), r.Uint64())
	m.bootID = fmt.Sprintf("%s-%s-%s-%s-%s", hex[:8], hex[8:12], hex[12:16], hex[16:20], hex[20:])
	m.regs = slices.Clone(m.cfg.BIOS)
	m.crashed = false
	m.wroteThisBoot = false
	clear(m.corruptPending)
	for _, mce := range m.queued {
		m.logMCE(m.bootID, mce, m.now)
	}
	m.queued = nil
	kind := m.nextReset
	m.reasons[m.bootID] = resetReason(kind, !m.cfg.OldKernel)
	m.nextReset = ""
}

func (m *Machine) Seams() machine.Machine {
	return machine.Machine{Clock: m, SMU: smu{m}, Host: host{m}, Trials: trials{m}, Kernel: kernel{m}}
}

func (m *Machine) Now() time.Time { return m.now.Add(m.wallOffset) }

func (m *Machine) Monotonic() time.Duration { return m.now.Sub(m.bootAt) }

func (m *Machine) JumpWall(d time.Duration) { m.wallOffset += d }

func (m *Machine) Sleep(ctx context.Context, d time.Duration) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if m.crashed {
		return machine.ErrCrashed
	}
	if d > 0 {
		m.now = m.now.Add(d)
	}
	return nil
}

func (m *Machine) IsolatedEdge(core int) int { return slices.Max(m.edges[core].Isolated[:]) }

func (m *Machine) ResidentEdge(core int) int { return slices.Max(m.edges[core].Resident[:]) }

func (m *Machine) Crash() {
	if m.nextReset == "" {
		m.nextReset = machine.ResetWatchdog
	}
	m.crashed = true
}

func (m *Machine) NextReset(kind machine.ResetKind) { m.nextReset = kind }

func (m *Machine) PowerLoss() { m.NextReset(machine.ResetPowerLoss); m.Crash() }

func (m *Machine) HoldPowerButton() { m.NextReset(machine.ResetPowerButton); m.Crash() }

func (m *Machine) Reboot() {
	m.now = m.now.Add(RebootTime)
	m.startBoot()
}

func (m *Machine) Violations() []string { return m.violations }

func (m *Machine) FailWrite()        { m.failWrite = true }
func (m *Machine) FailWriteAt(k int) { m.failWriteAt = k }

func (m *Machine) CrashAtWrite(k int) { m.crashAtWrite = k }

func (m *Machine) MissBackend(b machine.Backend) { m.missingBackends[b] = true }

func (m *Machine) CorruptReadback(core int) { m.corruptArmed[core] = true }

func (m *Machine) FailSetup(n int) { m.failSetup = n }

func (m *Machine) Escape() { m.escape = true }

func (m *Machine) FailCheck(name, detail string) {
	if detail == "" {
		delete(m.failedChecks, name)
		return
	}
	m.failedChecks[name] = detail
}

func (m *Machine) SetBIOSContext(c machine.BIOSContext) { m.bios = c }

func (m *Machine) CrashBeforeApply(boots int) { m.crashBeforeApply = boots }

type smu struct{ m *Machine }

func (s smu) Offset(core int) (int, error) {
	m := s.m
	if m.crashed {
		return 0, machine.ErrCrashed
	}
	if core < 0 || core >= len(m.regs) {
		return 0, fmt.Errorf("simulated SMU: no core %d", core)
	}
	o := m.regs[core]
	if m.corruptPending[core] {
		delete(m.corruptPending, core)
		if o == machine.MinOffset {
			return o + 1, nil
		}
		return o - 1, nil
	}
	return o, nil
}

func (s smu) SetOffset(core, offset int) error {
	m := s.m
	if core < 0 || core >= len(m.regs) {
		return fmt.Errorf("simulated SMU: no core %d", core)
	}
	if err := m.write(offset); err != nil {
		return err
	}
	m.regs[core] = offset
	m.written(core)
	return nil
}

func (s smu) SetAllOffsets(offset int) error {
	m := s.m
	if err := m.write(offset); err != nil {
		return err
	}
	for c := range m.regs {
		m.regs[c] = offset
		m.written(c)
	}
	return nil
}

func (m *Machine) write(offset int) error {
	if m.crashed {
		return machine.ErrCrashed
	}
	if offset < machine.MinOffset || offset > machine.MaxOffset {
		return fmt.Errorf("simulated SMU: offset %d outside [-50, 0]", offset)
	}
	first := !m.wroteThisBoot
	if m.crashAtWrite > 0 {
		m.crashAtWrite--
		if m.crashAtWrite == 0 {
			m.Crash()
			return machine.ErrCrashed
		}
	}
	if m.failWriteAt > 0 {
		m.failWriteAt--
		if m.failWriteAt == 0 {
			return errors.New("simulated SMU command failure")
		}
	}
	m.wroteThisBoot = true
	if first && m.crashBeforeApply > 0 {
		m.crashBeforeApply--
		m.Crash()
		return machine.ErrCrashed
	}
	if m.failWrite {
		m.failWrite = false
		return errors.New("simulated SMU command failure")
	}
	return nil
}

func (m *Machine) written(core int) {
	if m.corruptArmed[core] {
		delete(m.corruptArmed, core)
		m.corruptPending[core] = true
	}
}

type host struct{ m *Machine }

func (h host) BootID() (string, error) {
	if h.m.crashed {
		return "", machine.ErrCrashed
	}
	return h.m.bootID, nil
}

func (h host) Topology() ([]machine.CoreInfo, error) {
	if h.m.crashed {
		return nil, machine.ErrCrashed
	}
	n := h.m.cfg.Cores
	cores := make([]machine.CoreInfo, n)
	for c := range cores {
		cores[c] = machine.CoreInfo{Core: c, CCD: c / (n / 2), CPUs: []int{c, c + n}}
	}
	return cores, nil
}

func (h host) Ranking() ([]int, error) {
	if h.m.crashed {
		return nil, machine.ErrCrashed
	}
	if h.m.cfg.Ranking == nil {
		return nil, errors.New("simulated preferred-core ranking unavailable")
	}
	return slices.Clone(h.m.cfg.Ranking), nil
}

func (h host) BIOSContext() (machine.BIOSContext, error) {
	if h.m.crashed {
		return machine.BIOSContext{}, machine.ErrCrashed
	}
	return h.m.bios, nil
}

var checkNames = []string{"root", "cpu", "ryzen_smu", "readback", "slot_mapping", "backends", "systemd_run"}

func (h host) Preflight() []machine.Check {
	checks := make([]machine.Check, len(checkNames))
	for i, name := range checkNames {
		checks[i] = machine.Check{Name: name, Detail: "simulated", OK: true}
		if detail, failed := h.m.failedChecks[name]; failed {
			checks[i] = machine.Check{Name: name, Detail: detail}
		}
	}
	return checks
}

type kernel struct{ m *Machine }

func (k kernel) MCEs(boot string, since time.Duration) ([]machine.MCE, error) {
	if k.m.crashed {
		return nil, machine.ErrCrashed
	}
	var out []machine.MCE
	for _, mce := range k.m.logs[boot] {
		if mce.Monotonic >= since {
			out = append(out, mce)
		}
	}
	return out, nil
}

func (k kernel) ResetReason(boot string) (machine.ResetReason, error) {
	if k.m.crashed {
		return machine.ResetReason{}, machine.ErrCrashed
	}
	reason, ok := k.m.reasons[boot]
	if !ok {
		return machine.ResetReason{}, fmt.Errorf("read reset reason of boot %s: %w", boot, machine.ErrBootMissing)
	}
	return reason, nil
}

func resetReason(kind machine.ResetKind, supported bool) machine.ResetReason {
	if !supported {
		return machine.ResetReason{}
	}
	reason := machine.ResetReason{Kind: kind, Supported: true}
	var text string
	var code uint32
	switch kind {
	case machine.ResetWatchdog:
		code, text = 1, "hardware watchdog timer expired"
	case machine.ResetSyncFlood:
		code, text = 2, "an uncorrected error caused a data fabric sync flood event"
	case machine.ResetCPUShutdown:
		code, text = 3, "internal CPU shutdown event occurred"
	case machine.ResetPowerButton:
		code, text = 4, "power button was pressed for 4 seconds"
	case machine.ResetThermalTrip:
		code, text = 5, "thermal pin BP_THERMTRIP_L was tripped"
	case machine.ResetUnknown:
		code, text = 6, "unrecognized reset reason"
	case machine.ResetPowerLoss:
		reason.Kind = ""
	}
	if text != "" {
		reason.Raw = fmt.Sprintf("x86/amd: Previous system reset reason [0x%08x]: %s", code, text)
	}
	return reason
}
