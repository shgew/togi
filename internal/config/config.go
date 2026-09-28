// Package config loads and validates the TOML configuration.
package config

import (
	"errors"
	"fmt"
	"maps"
	"math"
	"path/filepath"
	"slices"
	"strconv"
	"strings"

	"github.com/BurntSushi/toml"

	"github.com/shgew/togi/internal/machine"
)

const DefaultPath = "/etc/togi/config.toml"

type Config struct {
	StartOffsets   map[int]int `json:"start_offsets"`
	CandidateEdges map[int]int `json:"candidate_edges"`
	Durations      Durations   `json:"durations"`
	Evidence       Evidence    `json:"evidence"`
	Guard          Guard       `json:"guard"`
	DeadEnds       DeadEnds    `json:"dead_ends"`
	Backends       Backends    `json:"backends"`
}

type Durations struct {
	SearchTrialS  int `toml:"search_trial_s" json:"search_trial_s"`
	StartS        int `toml:"start_s" json:"start_s"`
	GuardTrialS   int `toml:"guard_trial_s" json:"guard_trial_s"`
	GuardIdleS    int `toml:"guard_idle_s" json:"guard_idle_s"`
	GuardAllCoreS int `toml:"guard_all_core_s" json:"guard_all_core_s"`
}

type Evidence struct {
	Miss float64 `toml:"miss" json:"miss"`
	Rate float64 `toml:"rate" json:"rate"`
}

func (e Evidence) Starts() int {
	return int(math.Ceil(math.Log(e.Miss) / math.Log1p(-e.Rate)))
}

type Guard struct {
	Rotation []machine.Regime `toml:"rotation" json:"rotation"`
}

type DeadEnds struct {
	InconclusiveInARow int `toml:"inconclusive_in_a_row" json:"inconclusive_in_a_row"`
	StrayCrashesInARow int `toml:"stray_crashes_in_a_row" json:"stray_crashes_in_a_row"`
}

type Backends struct {
	Mprime    string `toml:"mprime" json:"mprime"`
	Ycruncher string `toml:"ycruncher" json:"ycruncher"`
}

func Default() Config {
	return Config{
		StartOffsets:   map[int]int{},
		CandidateEdges: map[int]int{},
		Durations: Durations{
			SearchTrialS:  90,
			StartS:        120,
			GuardTrialS:   120,
			GuardIdleS:    900,
			GuardAllCoreS: 1200,
		},
		Evidence: Evidence{Miss: 0.05, Rate: 0.5},
		Guard: Guard{
			Rotation: []machine.Regime{machine.R7, machine.R7, machine.R7, machine.R2, machine.R2, machine.R2, machine.R6, machine.R5, machine.R1, machine.R1, machine.R1, machine.R3, machine.R4, machine.R6},
		},
		DeadEnds: DeadEnds{
			InconclusiveInARow: 3,
			StrayCrashesInARow: 3,
		},
	}
}

type file struct {
	StartOffsets   map[string]int `toml:"start_offsets"`
	CandidateEdges map[string]int `toml:"candidate_edges"`
	Durations      *Durations     `toml:"durations"`
	Evidence       *Evidence      `toml:"evidence"`
	Guard          *Guard         `toml:"guard"`
	DeadEnds       *DeadEnds      `toml:"dead_ends"`
	Backends       *Backends      `toml:"backends"`
}

func Load(path string) (Config, error) {
	c, err := load(path)
	if err != nil {
		return Config{}, fmt.Errorf("load config %s: %w", path, err)
	}
	return c, nil
}

func load(path string) (Config, error) {
	c := Default()
	f := file{Durations: &c.Durations, Evidence: &c.Evidence, Guard: &c.Guard, DeadEnds: &c.DeadEnds, Backends: &c.Backends}
	md, err := toml.DecodeFile(path, &f)
	if err != nil {
		return Config{}, err
	}
	if md.IsDefined("durations", "confirmation_trial_s") {
		return Config{}, errors.New("durations.confirmation_trial_s was removed in togi 0.5.0: confirmation no longer exists; delete the key")
	}
	if undecoded := md.Undecoded(); len(undecoded) > 0 {
		keys := make([]string, len(undecoded))
		for i, k := range undecoded {
			keys[i] = k.String()
		}
		return Config{}, fmt.Errorf("unknown keys: %s", strings.Join(keys, ", "))
	}
	if err := convertOffsets("start_offsets", f.StartOffsets, c.StartOffsets); err != nil {
		return Config{}, err
	}
	if err := convertOffsets("candidate_edges", f.CandidateEdges, c.CandidateEdges); err != nil {
		return Config{}, err
	}
	if err := validate(c); err != nil {
		return Config{}, err
	}
	return c, nil
}

func convertOffsets(table string, in map[string]int, out map[int]int) error {
	keys := slices.Sorted(maps.Keys(in))
	for _, key := range keys {
		core, err := strconv.Atoi(key)
		if err != nil || core < 0 || strconv.Itoa(core) != key {
			return fmt.Errorf("%s.%q: core must be a non-negative integer", table, key)
		}
		out[core] = in[key]
	}
	for _, key := range keys {
		if v := in[key]; v < machine.MinOffset || v > machine.MaxOffset {
			return fmt.Errorf("%s.%q = %d: offset must be within [%d, %d]", table, key, v, machine.MinOffset, machine.MaxOffset)
		}
	}
	return nil
}

type intField struct {
	key   string
	value int
}

func validate(c Config) error {
	durations := []struct {
		key        string
		value, min int
	}{
		{"search_trial_s", c.Durations.SearchTrialS, 1},
		{"start_s", c.Durations.StartS, 1},
		{"guard_trial_s", c.Durations.GuardTrialS, 1},
		{"guard_idle_s", c.Durations.GuardIdleS, 1},
		{"guard_all_core_s", c.Durations.GuardAllCoreS, 4},
	}
	for _, d := range durations {
		if d.value < d.min || d.value > 86400 {
			return fmt.Errorf("durations.%s = %d: must be within [%d, 86400]", d.key, d.value, d.min)
		}
	}
	if math.IsNaN(c.Evidence.Miss) || math.IsInf(c.Evidence.Miss, 0) || c.Evidence.Miss <= 0 || c.Evidence.Miss >= 1 {
		return fmt.Errorf("evidence.miss = %g: must be within (0, 1)", c.Evidence.Miss)
	}
	if math.IsNaN(c.Evidence.Rate) || math.IsInf(c.Evidence.Rate, 0) || c.Evidence.Rate <= 0 || c.Evidence.Rate >= 1 {
		return fmt.Errorf("evidence.rate = %g: must be within (0, 1)", c.Evidence.Rate)
	}
	starts := math.Log(c.Evidence.Miss) / math.Log1p(-c.Evidence.Rate)
	if math.IsNaN(starts) || math.IsInf(starts, 0) || starts > 1000 {
		return fmt.Errorf("evidence: miss %g and rate %g need more than 1000 starts per step", c.Evidence.Miss, c.Evidence.Rate)
	}
	for _, core := range slices.Sorted(maps.Keys(c.CandidateEdges)) {
		if _, ok := c.StartOffsets[core]; ok {
			return fmt.Errorf("start_offsets.\"%d\" and candidate_edges.\"%d\": set at most one per core", core, core)
		}
	}
	if len(c.Guard.Rotation) == 0 {
		return errors.New("guard.rotation: must not be empty")
	}
	for i, r := range c.Guard.Rotation {
		if !r.Valid() {
			return fmt.Errorf("guard.rotation[%d] = %q: not a regime", i, r)
		}
	}
	thresholds := []intField{
		{"inconclusive_in_a_row", c.DeadEnds.InconclusiveInARow},
		{"stray_crashes_in_a_row", c.DeadEnds.StrayCrashesInARow},
	}
	for _, t := range thresholds {
		if t.value < 1 || t.value > 100 {
			return fmt.Errorf("dead_ends.%s = %d: must be within [1, 100]", t.key, t.value)
		}
	}
	backends := []struct{ key, path string }{
		{"mprime", c.Backends.Mprime},
		{"ycruncher", c.Backends.Ycruncher},
	}
	for _, b := range backends {
		if b.path != "" && !filepath.IsAbs(b.path) {
			return fmt.Errorf("backends.%s = %q: must be an absolute path", b.key, b.path)
		}
	}
	return nil
}
