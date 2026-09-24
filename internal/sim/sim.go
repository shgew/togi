package sim

import (
	"errors"
	"fmt"
	"hash/fnv"
	"math"
	"math/rand/v2"
	"slices"
	"strings"
	"time"

	"code.marleb.org/shgew/shycler/internal/machine"
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
	Start time.Time
}

// Edges index 0 is R1; Resident[5] and Resident[6] are R6 and R7.
type Edges struct {
	Isolated [5]int
	Resident [7]int
}

type Model struct {
	PastEdgeRate  float64
	Growth        float64
	NearEdgeRate  float64
	Signals       map[machine.Signal]float64
	CrashMCE      float64
	CoreLocalBank float64
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
	}
}

var defaultBIOSContext = machine.BIOSContext{
	BIOSVersion:   "SIM.1",
	Board:         "shycler simulator",
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
	crashed    bool
	logs       map[string][]machine.MCE
	queued     []machine.MCE
	violations []string
	bios       machine.BIOSContext

	failWrite        bool
	corruptArmed     map[int]bool
	corruptPending   map[int]bool
	failSetup        int
	escape           bool
	failedChecks     map[string]string
	crashBeforeApply int
	wroteThisBoot    bool
}

func New(cfg Config) (*Machine, error) {
	if cfg.Cores == 0 {
		cfg.Cores = 16
	}
	if cfg.Cores < 2 || cfg.Cores%2 != 0 {
		return nil, fmt.Errorf("new simulator: %d cores: must be even and at least 2", cfg.Cores)
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
	m := &Machine{
		cfg:            cfg,
		model:          model,
		edges:          cfg.Edges,
		boot:           cfg.Boots,
		now:            start,
		logs:           map[string][]machine.MCE{},
		bios:           cfg.BIOSContext,
		corruptArmed:   map[int]bool{},
		corruptPending: map[int]bool{},
		failedChecks:   map[string]string{},
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
	m.startBoot()
	return m, nil
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
}

func (m *Machine) Seams() machine.Machine {
	return machine.Machine{Clock: m, SMU: smu{m}, Host: host{m}, Trials: trials{m}, Kernel: kernel{m}}
}

func (m *Machine) Now() time.Time { return m.now }

func (m *Machine) IsolatedEdge(core int) int { return slices.Max(m.edges[core].Isolated[:]) }

func (m *Machine) ResidentEdge(core int) int { return slices.Max(m.edges[core].Resident[:]) }

func (m *Machine) Crash() { m.crashed = true }

func (m *Machine) Reboot() {
	m.now = m.now.Add(RebootTime)
	m.startBoot()
}

func (m *Machine) Violations() []string { return m.violations }

func (m *Machine) FailWrite() { m.failWrite = true }

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

func (k kernel) MCEs(boot string, since time.Time) ([]machine.MCE, error) {
	if k.m.crashed {
		return nil, machine.ErrCrashed
	}
	var out []machine.MCE
	for _, mce := range k.m.logs[boot] {
		if !mce.Time.Before(since) {
			out = append(out, mce)
		}
	}
	return out, nil
}
