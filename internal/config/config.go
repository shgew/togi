// Package config loads and validates the JSON configuration.
package config

import (
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"fmt"
	"maps"
	"math"
	"os"
	"path/filepath"
	"slices"
	"strconv"

	"github.com/shgew/togi/internal/machine"
)

const DefaultPath = "/etc/togi/config.json"

type Config struct {
	StartOffsets        map[int]int `json:"start_offsets"`
	CandidateSoloLimits map[int]int `json:"candidate_solo_limits"`
	Durations           Durations   `json:"durations"`
	Evidence            Evidence    `json:"evidence"`
	Checking            Checking    `json:"checking"`
	DeadEnds            DeadEnds    `json:"dead_ends"`
	Backends            Backends    `json:"backends"`
	BackendUser         string      `json:"backend_user"`
}

type Durations struct {
	SearchTrialS     int `json:"search_trial_s"`
	ShortTrialS      int `json:"short_trial_s"`
	CheckingTrialS   int `json:"checking_trial_s"`
	CheckingIdleS    int `json:"checking_idle_s"`
	CheckingAllCoreS int `json:"checking_all_core_s"`
}

type Evidence struct {
	Miss float64 `json:"miss"`
	Rate float64 `json:"rate"`
}

func (e Evidence) Trials() int {
	return int(math.Ceil(math.Log(e.Miss) / math.Log1p(-e.Rate)))
}

type Checking struct {
	Cycle []machine.Regime `json:"cycle"`
}

type DeadEnds struct {
	InconclusiveInARow int `json:"inconclusive_in_a_row"`
	StrayCrashesInARow int `json:"stray_crashes_in_a_row"`
}

type Backends struct {
	Mprime    string `json:"mprime"`
	Ycruncher string `json:"ycruncher"`
}

func Default() Config {
	return Config{
		StartOffsets:        map[int]int{},
		CandidateSoloLimits: map[int]int{},
		Durations: Durations{
			SearchTrialS:     90,
			ShortTrialS:      120,
			CheckingTrialS:   120,
			CheckingIdleS:    900,
			CheckingAllCoreS: 1200,
		},
		Evidence: Evidence{Miss: 0.05, Rate: 0.5},
		Checking: Checking{
			Cycle: []machine.Regime{machine.R7, machine.R7, machine.R7, machine.R2, machine.R2, machine.R2, machine.R6, machine.R5, machine.R1, machine.R1, machine.R1, machine.R3, machine.R3, machine.R3, machine.R4, machine.R4, machine.R4, machine.R6},
		},
		DeadEnds: DeadEnds{
			InconclusiveInARow: 3,
			StrayCrashesInARow: 3,
		},
	}
}

type file struct {
	StartOffsets        map[string]int `json:"start_offsets"`
	CandidateSoloLimits map[string]int `json:"candidate_solo_limits"`
	Durations           *Durations     `json:"durations"`
	Evidence            *Evidence      `json:"evidence"`
	Checking            *Checking      `json:"checking"`
	DeadEnds            *DeadEnds      `json:"dead_ends"`
	Backends            *Backends      `json:"backends"`
	BackendUser         *string        `json:"backend_user"`
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
	f := file{Durations: &c.Durations, Evidence: &c.Evidence, Checking: &c.Checking, DeadEnds: &c.DeadEnds, Backends: &c.Backends, BackendUser: &c.BackendUser}
	data, err := os.ReadFile(path)
	if err != nil {
		return Config{}, err
	}
	var removed struct {
		Durations struct {
			ConfirmationTrialS *jsontext.Value `json:"confirmation_trial_s"`
		} `json:"durations"`
	}
	if json.Unmarshal(data, &removed) == nil && removed.Durations.ConfirmationTrialS != nil {
		return Config{}, errors.New("durations.confirmation_trial_s was removed in togi 0.5.0: confirmation no longer exists; delete the key")
	}
	if err := json.Unmarshal(data, &f, json.RejectUnknownMembers(true)); err != nil {
		return Config{}, err
	}
	if err := convertOffsets("start_offsets", f.StartOffsets, c.StartOffsets); err != nil {
		return Config{}, err
	}
	if err := convertOffsets("candidate_solo_limits", f.CandidateSoloLimits, c.CandidateSoloLimits); err != nil {
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
		{"short_trial_s", c.Durations.ShortTrialS, 1},
		{"checking_trial_s", c.Durations.CheckingTrialS, 1},
		{"checking_idle_s", c.Durations.CheckingIdleS, 1},
		{"checking_all_core_s", c.Durations.CheckingAllCoreS, 4},
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
	trials := math.Log(c.Evidence.Miss) / math.Log1p(-c.Evidence.Rate)
	if math.IsNaN(trials) || math.IsInf(trials, 0) || trials > 1000 {
		return fmt.Errorf("evidence: miss %g and rate %g need more than 1000 trials per step", c.Evidence.Miss, c.Evidence.Rate)
	}
	for _, core := range slices.Sorted(maps.Keys(c.CandidateSoloLimits)) {
		if _, ok := c.StartOffsets[core]; ok {
			return fmt.Errorf("start_offsets.\"%d\" and candidate_solo_limits.\"%d\": set at most one per core", core, core)
		}
	}
	if len(c.Checking.Cycle) == 0 {
		return errors.New("checking.cycle: must not be empty")
	}
	for i, r := range c.Checking.Cycle {
		if !r.Valid() {
			return fmt.Errorf("checking.cycle[%d] = %q: not a regime", i, r)
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
