package sim

import (
	"cmp"
	"fmt"
	"strconv"

	"github.com/BurntSushi/toml"
	"github.com/shgew/togi/internal/machine"
)

func LoadMachine(path string) (Config, error) {
	var f struct {
		Cores       int   `toml:"cores"`
		BIOS        []int `toml:"bios"`
		Ranking     []int `toml:"ranking"`
		OldKernel   bool  `toml:"old_kernel"`
		BIOSContext *struct {
			BIOSVersion   string `toml:"bios_version"`
			Board         string `toml:"board"`
			CPUModel      string `toml:"cpu_model"`
			Microcode     string `toml:"microcode"`
			BoostLimitMHz int    `toml:"boost_limit_mhz"`
		} `toml:"bios_context"`
		Model struct {
			PastEdgeRate  *float64                      `toml:"past_edge_rate"`
			Growth        *float64                      `toml:"growth"`
			NearEdgeRate  *float64                      `toml:"near_edge_rate"`
			CrashMCE      *float64                      `toml:"crash_mce"`
			CoreLocalBank *float64                      `toml:"core_local_bank"`
			OnsetS        *float64                      `toml:"onset_s"`
			OnsetBoost    *float64                      `toml:"onset_boost"`
			Signals       map[machine.Signal]float64    `toml:"signals"`
			Reset         map[machine.ResetKind]float64 `toml:"reset"`
		} `toml:"model"`
		Core []struct {
			ID       int            `toml:"id"`
			Isolated []int          `toml:"isolated"`
			Resident []int          `toml:"resident"`
			Idle     *int           `toml:"idle"`
			Workload map[string]int `toml:"workload"`
			Flat     float64        `toml:"flat"`
		} `toml:"core"`
		Joint []struct {
			Members map[string]int   `toml:"members"`
			Regimes []machine.Regime `toml:"regimes"`
			Rate    float64          `toml:"rate"`
			AfterS  float64          `toml:"after_s"`
			Signal  machine.Signal   `toml:"signal"`
		} `toml:"joint"`
		Script []struct {
			Trial     string            `toml:"trial"`
			Signal    machine.Signal    `toml:"signal"`
			AtS       float64           `toml:"at_s"`
			Core      int               `toml:"core"`
			Reset     machine.ResetKind `toml:"reset"`
			ThenCrash bool              `toml:"then_crash"`
		} `toml:"script"`
	}
	md, err := toml.DecodeFile(path, &f)
	if err != nil {
		return Config{}, fmt.Errorf("load simulator machine %s: %w", path, err)
	}
	if keys := md.Undecoded(); len(keys) != 0 {
		return Config{}, fmt.Errorf("load simulator machine %s: unknown key %s", path, keys[0])
	}
	cfg := Config{Cores: f.Cores, BIOS: f.BIOS, Ranking: f.Ranking, OldKernel: f.OldKernel}
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
	set(&model.PastEdgeRate, f.Model.PastEdgeRate)
	set(&model.Growth, f.Model.Growth)
	set(&model.NearEdgeRate, f.Model.NearEdgeRate)
	set(&model.CrashMCE, f.Model.CrashMCE)
	set(&model.CoreLocalBank, f.Model.CoreLocalBank)
	set(&model.OnsetS, f.Model.OnsetS)
	set(&model.OnsetBoost, f.Model.OnsetBoost)
	if f.Model.Signals != nil {
		model.Signals = f.Model.Signals
	}
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
		cfg.Edges = make([]Edges, cfg.Cores)
		seen := make([]bool, cfg.Cores)
		for _, core := range f.Core {
			if core.ID < 0 || core.ID >= cfg.Cores || seen[core.ID] {
				return Config{}, fmt.Errorf("load simulator machine %s: invalid or duplicate core %d", path, core.ID)
			}
			if len(core.Isolated) != 5 || len(core.Resident) != 7 {
				return Config{}, fmt.Errorf("load simulator machine %s: core %d needs five isolated and seven resident edges", path, core.ID)
			}
			seen[core.ID] = true
			var e Edges
			copy(e.Isolated[:], core.Isolated)
			copy(e.Resident[:], core.Resident)
			e.Idle, e.Workload, e.Flat = core.Idle, core.Workload, core.Flat
			cfg.Edges[core.ID] = e
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
		cfg.Joints = append(cfg.Joints, Joint{Members: members, Regimes: j.Regimes, Rate: j.Rate, AfterS: j.AfterS, Signal: j.Signal})
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
	if _, err := New(cfg); err != nil {
		return Config{}, fmt.Errorf("load simulator machine %s: %w", path, err)
	}
	return cfg, nil
}
