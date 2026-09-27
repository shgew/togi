// Package config loads and validates the TOML configuration.
package config

import (
	"errors"
	"fmt"
	"maps"
	"path/filepath"
	"slices"
	"strconv"
	"strings"

	"github.com/BurntSushi/toml"

	"github.com/shgew/shycler/internal/machine"
)

const DefaultPath = "/etc/shycler/config.toml"

type Config struct {
	StartOffsets   map[int]int `json:"start_offsets"`
	CandidateEdges map[int]int `json:"candidate_edges"`
	Durations      Durations   `json:"durations"`
	Guard          Guard       `json:"guard"`
	DeadEnds       DeadEnds    `json:"dead_ends"`
	Backends       Backends    `json:"backends"`
}

type Durations struct {
	SearchTrialS       int `toml:"search_trial_s" json:"search_trial_s"`
	ConfirmationTrialS int `toml:"confirmation_trial_s" json:"confirmation_trial_s"`
	GuardTrialS        int `toml:"guard_trial_s" json:"guard_trial_s"`
	GuardIdleS         int `toml:"guard_idle_s" json:"guard_idle_s"`
	GuardAllCoreS      int `toml:"guard_all_core_s" json:"guard_all_core_s"`
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
			SearchTrialS:       90,
			ConfirmationTrialS: 300,
			GuardTrialS:        120,
			GuardIdleS:         900,
			GuardAllCoreS:      1200,
		},
		Guard: Guard{
			Rotation: []machine.Regime{machine.R2, machine.R7, machine.R6, machine.R5, machine.R1, machine.R3, machine.R4, machine.R6},
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
	f := file{Durations: &c.Durations, Guard: &c.Guard, DeadEnds: &c.DeadEnds, Backends: &c.Backends}
	md, err := toml.DecodeFile(path, &f)
	if err != nil {
		return Config{}, err
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
		{"confirmation_trial_s", c.Durations.ConfirmationTrialS, 1},
		{"guard_trial_s", c.Durations.GuardTrialS, 1},
		{"guard_idle_s", c.Durations.GuardIdleS, 1},
		{"guard_all_core_s", c.Durations.GuardAllCoreS, 4},
	}
	for _, d := range durations {
		if d.value < d.min || d.value > 86400 {
			return fmt.Errorf("durations.%s = %d: must be within [%d, 86400]", d.key, d.value, d.min)
		}
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
