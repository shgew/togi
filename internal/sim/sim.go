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
	"strconv"
	"strings"
	"time"

	"github.com/shgew/togi/internal/machine"
)

type Config struct {
	Seed uint64
	// Cores defaults to 16; it must be even, at least 2 and at most 16 (machine.PerCoreMax). Core c is on CCD c/(Cores/2) with CPUs c and c+Cores.
	Cores  int
	Facts  string
	Replay *Replay
	// BIOS holds the offsets restored at every boot; default all 0.
	BIOS        []int
	BIOSContext machine.BIOSContext
	// Limits are the hidden limits; nil draws them from Seed.
	Limits []Limits
	// Model nil means DefaultModel(); a non-nil model is used verbatim, zero fields included.
	Model         *Model
	CCD           *CCD
	SharedVoltage *SharedVoltage
	// Boots counts the boots before the first; boot numbering and boot IDs continue from it.
	Boots int
	// Start is the clock at the first boot; zero means 2026-01-01T00:00:00Z.
	Start     time.Time
	Joints    []Joint
	Ranking   []int
	Script    map[string]Outcome
	OldKernel bool
}

// Limits index 0 is R1; Together[5] and Together[6] are R6 and R7.
type Limits struct {
	Alone    [5]int
	Together [7]int
	Idle     *int
	Workload map[string]int
	Flat     float64
}

// CCD adds a smooth R7 hazard for each loaded CCD, using its mean applied depth.
type CCD struct {
	LogRate float64    `json:"log_rate"`
	Slope   float64    `json:"slope"`
	Effect  [2]float64 `json:"effect"`
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
	PastLimitRate float64
	Growth        float64
	NearLimitRate float64
	Signals       map[machine.Signal]float64
	// RegimeSignals replaces Signals for failures in each regime it lists. When it lists any regime, joints without a
	// signal and [ccd] hazards draw from their regime's weights, else from Signals, instead of crashing.
	RegimeSignals map[machine.Regime]map[machine.Signal]float64
	CrashMCE      float64
	CoreLocalBank float64
	OnsetS        float64
	OnsetBoost    float64
	Reset         map[machine.ResetKind]float64
}

func DefaultModel() Model {
	return Model{
		PastLimitRate: math.Ln10 / 90,
		Growth:        4,
		NearLimitRate: 0,
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
	hazards
	regs       []int
	boot       int
	bootID     string
	boots      []string
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
	samples    trialSamples
	samplesDir string

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
	smuHook          func(SMUOperation) error
}

func validateCores(cores int) error {
	if cores < 2 || cores%2 != 0 || cores > machine.PerCoreMax {
		return fmt.Errorf("%d cores: must be even and at least 2, and at most %d", cores, machine.PerCoreMax)
	}
	return nil
}

func validateSignals(signals map[machine.Signal]float64) error {
	var total float64
	for _, signal := range slices.Sorted(maps.Keys(signals)) {
		weight := signals[signal]
		if !slices.Contains(signalOrder, signal) {
			return fmt.Errorf("signal %q is not supported", signal)
		}
		if weight < 0 {
			return fmt.Errorf("signal %q weight %g is negative", signal, weight)
		}
		total += weight
	}
	if !(total > 0) {
		return errors.New("weights must sum to a positive total")
	}
	return nil
}

func validateRegimeSignals(signals map[machine.Regime]map[machine.Signal]float64) error {
	for _, regime := range slices.Sorted(maps.Keys(signals)) {
		if !slices.Contains(machine.Regimes, regime) {
			return fmt.Errorf("model.regime_signals regime %q is not supported", regime)
		}
		if err := validateSignals(signals[regime]); err != nil {
			return fmt.Errorf("model.regime_signals.%s: %w", regime, err)
		}
	}
	return nil
}

// New builds a machine from its own copy of cfg: later changes to cfg, or to anything it references, never reach the machine.
func New(cfg Config) (*Machine, error) {
	cfg, model, err := resolve(cfg)
	if err != nil {
		return nil, fmt.Errorf("new simulator: %w", err)
	}
	start := epoch
	if !cfg.Start.IsZero() {
		start = cfg.Start
	}
	m := &Machine{
		cfg:             cfg,
		model:           model,
		limits:          cfg.Limits,
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
	if m.limits == nil {
		m.limits = m.drawLimits()
	}
	m.index(cfg.Joints)
	m.startBoot()
	return m, nil
}

// resolve returns a copy of cfg with its defaults applied and the failure model it selects, or the first invalid
// setting. Limits left nil stay nil: limits drawn from the seed are valid by construction.
func resolve(cfg Config) (Config, Model, error) {
	cfg = cfg.Clone()
	if cfg.Cores == 0 {
		cfg.Cores = 16
	}
	if err := validateCores(cfg.Cores); err != nil {
		return Config{}, Model{}, err
	}
	if cfg.BIOS == nil {
		cfg.BIOS = make([]int, cfg.Cores)
	}
	if err := validateBIOS(cfg.BIOS, cfg.Cores); err != nil {
		return Config{}, Model{}, err
	}
	if cfg.BIOSContext == (machine.BIOSContext{}) {
		cfg.BIOSContext = defaultBIOSContext
	}
	model := DefaultModel()
	if cfg.Model != nil {
		model = *cfg.Model
	}
	if err := validateSignals(model.Signals); err != nil {
		return Config{}, Model{}, fmt.Errorf("model.signals: %w", err)
	}
	if err := validateRegimeSignals(model.RegimeSignals); err != nil {
		return Config{}, Model{}, err
	}
	if err := validateCCD(cfg.CCD); err != nil {
		return Config{}, Model{}, err
	}
	voltage, err := normalizeVoltage(cfg.SharedVoltage, cfg.Cores)
	if err != nil {
		return Config{}, Model{}, err
	}
	cfg.SharedVoltage = voltage
	if err := validateResets(model.Reset); err != nil {
		return Config{}, Model{}, err
	}
	for _, trial := range slices.Sorted(maps.Keys(cfg.Script)) {
		if err := validateOutcome(trial, cfg.Script[trial], cfg.Cores); err != nil {
			return Config{}, Model{}, err
		}
	}
	if err := validateLimits(cfg.Limits, cfg.Cores); err != nil {
		return Config{}, Model{}, err
	}
	if cfg.Ranking != nil && len(cfg.Ranking) != cfg.Cores {
		return Config{}, Model{}, fmt.Errorf("%d ranking values for %d cores", len(cfg.Ranking), cfg.Cores)
	}
	if err := validateLimitHazards(cfg.Limits); err != nil {
		return Config{}, Model{}, err
	}
	if err := validateJoints(cfg.Joints, cfg.Cores); err != nil {
		return Config{}, Model{}, err
	}
	return cfg, model, nil
}

func validateBIOS(bios []int, cores int) error {
	if len(bios) != cores {
		return fmt.Errorf("%d BIOS offsets for %d cores", len(bios), cores)
	}
	for c, o := range bios {
		if o < machine.MinOffset || o > machine.MaxOffset {
			return fmt.Errorf("BIOS offset %d of core %d outside [-50, 0]", o, c)
		}
	}
	return nil
}

func validateCCD(c *CCD) error {
	if c == nil {
		return nil
	}
	for _, x := range []float64{c.LogRate, c.Slope, c.Effect[0], c.Effect[1]} {
		if math.IsNaN(x) || math.IsInf(x, 0) {
			return errors.New("ccd parameters must be finite")
		}
	}
	if c.Slope < 0 {
		return errors.New("ccd slope must be nonnegative")
	}
	return nil
}

func validateResets(weights map[machine.ResetKind]float64) error {
	for _, kind := range slices.Sorted(maps.Keys(weights)) {
		weight := weights[kind]
		if !slices.Contains(resetOrder, kind) {
			return fmt.Errorf("reset %q is not supported", kind)
		}
		if weight < 0 {
			return fmt.Errorf("reset %q weight %g is negative", kind, weight)
		}
	}
	return nil
}

func validateLimits(limits []Limits, cores int) error {
	if limits != nil && len(limits) != cores {
		return fmt.Errorf("%d limits for %d cores", len(limits), cores)
	}
	for c := range limits {
		for _, values := range [][]int{limits[c].Alone[:], limits[c].Together[:]} {
			for _, v := range values {
				if v < machine.MinOffset || v > 1 {
					return fmt.Errorf("limit %d of core %d outside [-50, 1]", v, c)
				}
			}
		}
	}
	return nil
}

func validateLimitHazards(limits []Limits) error {
	for c := range limits {
		e := &limits[c]
		if e.Idle != nil && (*e.Idle < machine.MinOffset || *e.Idle > 1) {
			return fmt.Errorf("idle limit %d of core %d outside [-50, 1]", *e.Idle, c)
		}
		for workload, offset := range e.Workload {
			if offset < machine.MinOffset || offset > 1 {
				return fmt.Errorf("workload %s limit %d of core %d outside [-50, 1]", workload, offset, c)
			}
		}
		if e.Flat < 0 {
			return fmt.Errorf("flat rate %g of core %d is negative", e.Flat, c)
		}
	}
	return nil
}

// Clone returns a copy of cfg that shares no mutable state with it. A Replay's facts are fixed once NewReplay
// returns, so the copy of the Replay shares them.
func (cfg Config) Clone() Config {
	cfg.BIOS = slices.Clone(cfg.BIOS)
	cfg.Limits = slices.Clone(cfg.Limits)
	for c := range cfg.Limits {
		if idle := cfg.Limits[c].Idle; idle != nil {
			cfg.Limits[c].Idle = new(*idle)
		}
		cfg.Limits[c].Workload = maps.Clone(cfg.Limits[c].Workload)
	}
	if cfg.Model != nil {
		model := *cfg.Model
		model.Signals = maps.Clone(model.Signals)
		if model.RegimeSignals != nil {
			regimes := make(map[machine.Regime]map[machine.Signal]float64, len(model.RegimeSignals))
			for regime, signals := range model.RegimeSignals {
				regimes[regime] = maps.Clone(signals)
			}
			model.RegimeSignals = regimes
		}
		model.Reset = maps.Clone(model.Reset)
		cfg.Model = &model
	}
	if cfg.CCD != nil {
		ccd := *cfg.CCD
		cfg.CCD = &ccd
	}
	if cfg.SharedVoltage != nil {
		voltage := *cfg.SharedVoltage
		voltage.Workload = maps.Clone(voltage.Workload)
		for id, workload := range voltage.Workload {
			workload.Core = slices.Clone(workload.Core)
			for c := range workload.Core {
				workload.Core[c].Signals = maps.Clone(workload.Core[c].Signals)
			}
			voltage.Workload[id] = workload
		}
		cfg.SharedVoltage = &voltage
	}
	if cfg.Replay != nil {
		replay := *cfg.Replay
		cfg.Replay = &replay
	}
	cfg.Joints = slices.Clone(cfg.Joints)
	for j := range cfg.Joints {
		joint := &cfg.Joints[j]
		joint.Members = maps.Clone(joint.Members)
		joint.Regimes = slices.Clone(joint.Regimes)
		if core := joint.CrashMCECore; core != nil {
			joint.CrashMCECore = new(*core)
		}
	}
	cfg.Ranking = slices.Clone(cfg.Ranking)
	cfg.Script = maps.Clone(cfg.Script)
	return cfg
}

func validateOutcome(trial string, s Outcome, cores int) error {
	if s.Core < 0 || s.Core >= cores {
		return fmt.Errorf("script trial %s core %d outside [0, %d)", trial, s.Core, cores)
	}
	if s.Signal != "" && !slices.Contains(signalOrder, s.Signal) {
		return fmt.Errorf("script trial %s signal %q is not supported", trial, s.Signal)
	}
	if s.Reset != "" && !slices.Contains(resetOrder, s.Reset) {
		return fmt.Errorf("script trial %s reset %q is not supported", trial, s.Reset)
	}
	return nil
}

func validateJoints(joints []Joint, cores int) error {
	for j := range joints {
		for c, offset := range joints[j].Members {
			if err := validateJointMember(c, offset, cores); err != nil {
				return err
			}
		}
		if err := validateJointFields(&joints[j], cores); err != nil {
			return err
		}
	}
	return nil
}

func validateJointMember(core, offset, cores int) error {
	if core < 0 || core >= cores || offset < machine.MinOffset || offset > machine.MaxOffset {
		return fmt.Errorf("joint member core %d offset %d invalid", core, offset)
	}
	return nil
}

// validateJointFields checks everything about a joint except its members.
func validateJointFields(joint *Joint, cores int) error {
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
	return nil
}

func (m *Machine) drawLimits() []Limits {
	r := m.rng("edges")
	together := func() int {
		if r.Float64() < 0.75 {
			return 0
		}
		return 1 + r.IntN(3)
	}
	limits := make([]Limits, m.cfg.Cores)
	for c := range limits {
		base := -5 - r.IntN(36)
		deepest := machine.MinOffset
		for i := range limits[c].Alone {
			limits[c].Alone[i] = machine.ClampOffset(base + r.IntN(3))
			deepest = max(deepest, limits[c].Alone[i])
		}
		for i := range 5 {
			limits[c].Together[i] = machine.ClampOffset(limits[c].Alone[i] + together())
		}
		limits[c].Together[5] = machine.ClampOffset(deepest + together())
		limits[c].Together[6] = machine.ClampOffset(deepest + together())
	}
	return limits
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
	m.boots = append(m.boots, m.bootID)
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

func (m *Machine) AloneLimit(core int) int { return slices.Max(m.limits[core].Alone[:]) }

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

// ScriptTrial fixes the outcome of the trial with ID trial, as Config.Script does, replacing any earlier script for it.
func (m *Machine) ScriptTrial(trial string, outcome Outcome) error {
	if err := validateOutcome(trial, outcome, m.cfg.Cores); err != nil {
		return fmt.Errorf("script simulator trial: %w", err)
	}
	if m.cfg.Script == nil {
		m.cfg.Script = map[string]Outcome{}
	}
	m.cfg.Script[trial] = outcome
	return nil
}

type SMUOperation struct {
	Op     string
	Core   int
	Offset int
	After  bool
}

func (m *Machine) InterruptSMU(hook func(SMUOperation) error) {
	m.smuHook = hook
}

func (m *Machine) interruptSMU(op string, core, offset int, after bool) error {
	if m.smuHook == nil {
		return nil
	}
	return m.smuHook(SMUOperation{Op: op, Core: core, Offset: offset, After: after})
}

type smu struct{ m *Machine }

func (s smu) Offset(core int) (int, error) {
	m := s.m
	if m.crashed {
		return 0, machine.ErrCrashed
	}
	if core < 0 || core >= len(m.regs) {
		return 0, fmt.Errorf("simulated SMU: no core %d", core)
	}
	if err := m.interruptSMU("read", core, m.regs[core], false); err != nil {
		return 0, err
	}
	o := m.regs[core]
	if m.corruptPending[core] {
		delete(m.corruptPending, core)
		if o == machine.MinOffset {
			o++
		} else {
			o--
		}
	}
	return o, m.interruptSMU("read", core, o, true)
}

func (s smu) SetOffset(core, offset int) error {
	m := s.m
	if core < 0 || core >= len(m.regs) {
		return fmt.Errorf("simulated SMU: no core %d", core)
	}
	if err := m.write(offset); err != nil {
		return err
	}
	if err := m.interruptSMU("set", core, offset, false); err != nil {
		return err
	}
	m.regs[core] = offset
	m.written(core)
	return m.interruptSMU("set", core, offset, true)
}

func (s smu) SetAllOffsets(offset int) error {
	m := s.m
	if err := m.write(offset); err != nil {
		return err
	}
	if err := m.interruptSMU("set_all", -1, offset, false); err != nil {
		return err
	}
	for c := range m.regs {
		m.regs[c] = offset
		m.written(c)
	}
	return m.interruptSMU("set_all", -1, offset, true)
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

func (h host) Ranking() ([]machine.CoreRank, error) {
	if h.m.crashed {
		return nil, machine.ErrCrashed
	}
	if h.m.cfg.Ranking == nil {
		return nil, errors.New("simulated preferred-core ranking unavailable")
	}
	ranking := make([]machine.CoreRank, len(h.m.cfg.Ranking))
	for core, value := range h.m.cfg.Ranking {
		ranking[core] = machine.CoreRank{Core: core, Value: value}
	}
	return ranking, nil
}

func (h host) ValidateSMU() error {
	for _, name := range []string{"cpu", "ryzen_smu"} {
		if detail, failed := h.m.failedChecks[name]; failed {
			return fmt.Errorf("validate %s: %s", name, detail)
		}
	}
	return nil
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
	if h.m.cfg.SharedVoltage != nil {
		return append(checks, machine.Check{Name: "pm_table", Detail: "simulated shared-voltage model supplies per-core voltage requests and loaded-core clocks", OK: true})
	}
	return append(checks, machine.Check{Name: "pm_table", Detail: "pm_table version unavailable: simulator reports no per-core lanes", OK: true})
}

func (h host) Watchdog() machine.Check {
	if detail, failed := h.m.failedChecks["watchdog"]; failed {
		return machine.Check{Name: "watchdog", Detail: detail}
	}
	return machine.Check{Name: "watchdog", Detail: "simulated hardware watchdog active", OK: true}
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

func (k kernel) ReadMCEs(boot, cursor string) (machine.KernelRead, error) {
	if k.m.crashed {
		return machine.KernelRead{}, machine.ErrCrashed
	}
	if _, ok := k.m.reasons[boot]; !ok {
		return machine.KernelRead{}, fmt.Errorf("read kernel log of boot %s: %w", boot, machine.ErrBootMissing)
	}
	start := 0
	if cursor != "" {
		prefix := boot + ":"
		if !strings.HasPrefix(cursor, prefix) {
			return machine.KernelRead{}, machine.ErrCursorMissing
		}
		var err error
		start, err = strconv.Atoi(strings.TrimPrefix(cursor, prefix))
		if err != nil || start < 0 || start > len(k.m.logs[boot]) {
			return machine.KernelRead{}, machine.ErrCursorMissing
		}
	}
	mces := k.m.logs[boot][start:]
	if len(mces) == 0 {
		mces = nil
	}
	return machine.KernelRead{MCEs: slices.Clone(mces), Cursor: boot + ":" + strconv.Itoa(len(k.m.logs[boot]))}, nil
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

func (k kernel) ResetReasonAfter(boot string) (machine.ResetReason, error) {
	if k.m.crashed {
		return machine.ResetReason{}, machine.ErrCrashed
	}
	i := slices.Index(k.m.boots, boot)
	if i < 0 || i+1 >= len(k.m.boots) {
		return machine.ResetReason{}, fmt.Errorf("find system boot after %s: %w", boot, machine.ErrBootMissing)
	}
	return k.ResetReason(k.m.boots[i+1])
}

func (k kernel) SavedPstore(string) (*machine.PstoreRecord, error) {
	if k.m.crashed {
		return nil, machine.ErrCrashed
	}
	return nil, nil
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
