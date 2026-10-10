package sim

import (
	"cmp"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"fmt"
	"os"
	"strconv"

	"github.com/shgew/togi/internal/machine"
)

// machineFile is the JSON shape of a simulated machine file. Description says what a hand-written machine models;
// Notes carry a generated machine's provenance. Neither changes the simulated machine.
type machineFile struct {
	Description   string               `json:"description,omitzero"`
	Notes         []string             `json:"notes,omitzero"`
	Cores         int                  `json:"cores,omitzero"`
	Facts         string               `json:"facts,omitzero"`
	BIOS          []int                `json:"bios,omitzero"`
	Ranking       []int                `json:"ranking,omitzero"`
	OldKernel     bool                 `json:"old_kernel,omitzero"`
	BIOSContext   *machine.BIOSContext `json:"bios_context,omitzero"`
	Model         fileModel            `json:"model,omitzero"`
	CCD           *CCD                 `json:"ccd,omitzero"`
	Core          []fileCore           `json:"core,omitzero"`
	Joint         []fileJoint          `json:"joint,omitzero"`
	Script        []fileScript         `json:"script,omitzero"`
	SharedVoltage *SharedVoltage       `json:"shared_voltage,omitzero"`
}

// fileModel overrides DefaultModel field by field; an absent field keeps the default.
type fileModel struct {
	PastLimitRate *float64                                      `json:"past_limit_rate,omitzero"`
	Growth        *float64                                      `json:"growth,omitzero"`
	NearLimitRate *float64                                      `json:"near_limit_rate,omitzero"`
	CrashMCE      *float64                                      `json:"crash_mce,omitzero"`
	CoreLocalBank *float64                                      `json:"core_local_bank,omitzero"`
	OnsetS        *float64                                      `json:"onset_s,omitzero"`
	OnsetBoost    *float64                                      `json:"onset_boost,omitzero"`
	Signals       map[machine.Signal]float64                    `json:"signals,omitzero"`
	RegimeSignals map[machine.Regime]map[machine.Signal]float64 `json:"regime_signals,omitzero"`
	Reset         map[machine.ResetKind]float64                 `json:"reset,omitzero"`
}

type fileCore struct {
	ID       int            `json:"id"`
	Alone    []int          `json:"alone"`
	Together []int          `json:"together"`
	Flat     float64        `json:"flat"`
	Idle     *int           `json:"idle,omitzero"`
	Workload map[string]int `json:"workload,omitzero"`
}

type fileJoint struct {
	Regimes      []machine.Regime `json:"regimes"`
	Rate         float64          `json:"rate"`
	AfterS       float64          `json:"after_s"`
	Members      map[string]int   `json:"members"`
	Signal       machine.Signal   `json:"signal,omitzero"`
	CrashMCECore *int             `json:"crash_mce_core,omitzero"`
}

type fileScript struct {
	Trial     string            `json:"trial"`
	Signal    machine.Signal    `json:"signal,omitzero"`
	AtS       float64           `json:"at_s,omitzero"`
	Core      int               `json:"core,omitzero"`
	Reset     machine.ResetKind `json:"reset,omitzero"`
	ThenCrash bool              `json:"then_crash,omitzero"`
}

// encode renders indented JSON with sorted map keys, ending in a newline.
func (f machineFile) encode() ([]byte, error) {
	b, err := json.Marshal(f, json.Deterministic(true), jsontext.WithIndent("  "))
	if err != nil {
		return nil, fmt.Errorf("encode simulator machine: %w", err)
	}
	return append(b, '\n'), nil
}

func LoadMachine(path string) (Config, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return Config{}, fmt.Errorf("load simulator machine %s: %w", path, err)
	}
	var f machineFile
	if err := json.Unmarshal(data, &f, json.RejectUnknownMembers(true)); err != nil {
		return Config{}, fmt.Errorf("load simulator machine %s: %w", path, err)
	}
	cfg := Config{Cores: f.Cores, BIOS: f.BIOS, Ranking: f.Ranking, OldKernel: f.OldKernel, Facts: f.Facts}
	cfg.CCD = f.CCD
	cfg.SharedVoltage = f.SharedVoltage
	if b := f.BIOSContext; b != nil {
		cfg.BIOSContext = machine.BIOSContext{
			BIOSVersion:   cmp.Or(b.BIOSVersion, defaultBIOSContext.BIOSVersion),
			Board:         cmp.Or(b.Board, defaultBIOSContext.Board),
			CPUModel:      cmp.Or(b.CPUModel, defaultBIOSContext.CPUModel),
			Microcode:     cmp.Or(b.Microcode, defaultBIOSContext.Microcode),
			BoostLimitMHz: cmp.Or(b.BoostLimitMHz, defaultBIOSContext.BoostLimitMHz),
		}
	}
	model := DefaultModel()
	set := func(dst *float64, src *float64) {
		if src != nil {
			*dst = *src
		}
	}
	set(&model.PastLimitRate, f.Model.PastLimitRate)
	set(&model.Growth, f.Model.Growth)
	set(&model.NearLimitRate, f.Model.NearLimitRate)
	set(&model.CrashMCE, f.Model.CrashMCE)
	set(&model.CoreLocalBank, f.Model.CoreLocalBank)
	set(&model.OnsetS, f.Model.OnsetS)
	set(&model.OnsetBoost, f.Model.OnsetBoost)
	if f.Model.Signals != nil {
		model.Signals = f.Model.Signals
	}
	model.RegimeSignals = f.Model.RegimeSignals
	if f.Model.Reset != nil {
		model.Reset = f.Model.Reset
	}
	cfg.Model = &model
	if f.Core != nil {
		if cfg.Cores == 0 {
			cfg.Cores = 16
		}
		if err := validateCores(cfg.Cores); err != nil {
			return Config{}, fmt.Errorf("load simulator machine %s: new simulator: %w", path, err)
		}
		cfg.Limits = make([]Limits, cfg.Cores)
		seen := make([]bool, cfg.Cores)
		for _, core := range f.Core {
			if core.ID < 0 || core.ID >= cfg.Cores || seen[core.ID] {
				return Config{}, fmt.Errorf("load simulator machine %s: invalid or duplicate core %d", path, core.ID)
			}
			if len(core.Alone) != 5 || len(core.Together) != 7 {
				return Config{}, fmt.Errorf("load simulator machine %s: core %d needs five alone and seven together limits", path, core.ID)
			}
			seen[core.ID] = true
			var e Limits
			copy(e.Alone[:], core.Alone)
			copy(e.Together[:], core.Together)
			e.Idle, e.Workload, e.Flat = core.Idle, core.Workload, core.Flat
			cfg.Limits[core.ID] = e
		}
		for c, ok := range seen {
			if !ok {
				return Config{}, fmt.Errorf("load simulator machine %s: missing core %d", path, c)
			}
		}
	}
	for _, j := range f.Joint {
		members := make(map[int]int, len(j.Members))
		for name, offset := range j.Members {
			c, err := strconv.Atoi(name)
			if err != nil {
				return Config{}, fmt.Errorf("load simulator machine %s: joint member %q: %w", path, name, err)
			}
			members[c] = offset
		}
		cfg.Joints = append(cfg.Joints, Joint{Members: members, Regimes: j.Regimes, Rate: j.Rate, AfterS: j.AfterS, Signal: j.Signal, CrashMCECore: j.CrashMCECore})
	}
	if f.Script != nil {
		cfg.Script = make(map[string]Outcome, len(f.Script))
		for _, s := range f.Script {
			if s.Trial == "" {
				return Config{}, fmt.Errorf("load simulator machine %s: empty script trial", path)
			}
			if _, exists := cfg.Script[s.Trial]; exists {
				return Config{}, fmt.Errorf("load simulator machine %s: duplicate script trial %s", path, s.Trial)
			}
			cfg.Script[s.Trial] = Outcome{Signal: s.Signal, AtS: s.AtS, Core: s.Core, Reset: s.Reset, ThenCrash: s.ThenCrash}
		}
	}
	if _, _, err := resolve(cfg); err != nil {
		return Config{}, fmt.Errorf("load simulator machine %s: new simulator: %w", path, err)
	}
	return cfg, nil
}
