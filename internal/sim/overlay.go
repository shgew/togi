package sim

import (
	"encoding/json/v2"
	"fmt"
	"path/filepath"
	"slices"
)

// overlayFile is the whole of an overlay document. It is decoded strictly from the same bytes as the machine file,
// so any other machine member, whatever its value, is an unknown member. Description and Notes are metadata only.
type overlayFile struct {
	Description           string                  `json:"description,omitzero"`
	Notes                 []string                `json:"notes,omitzero"`
	Extends               string                  `json:"extends"`
	SharedVoltageOverride []sharedVoltageOverride `json:"shared_voltage_override"`
}

// sharedVoltageOverride replaces the required-voltage fields of one core of one workload in a parent machine's
// shared_voltage. At least one of the two fields must be set; an absent field keeps the parent's value.
type sharedVoltageOverride struct {
	Workload                 string   `json:"workload"`
	Core                     *int     `json:"core"`
	ThresholdV               *float64 `json:"threshold_v,omitzero"`
	ThresholdClockVPer100MHz *float64 `json:"threshold_clock_v_per_100mhz,omitzero"`
}

// loadOverlay materializes the parent that the document in data extends and applies its overrides to it. Everything
// the overlay does not name, including the model's signal mix, is the parent's, so a changed parent changes the overlay.
func loadOverlay(path string, data []byte, files *[]string, chain []string) (Config, error) {
	fail := func(format string, args ...any) (Config, error) {
		return Config{}, fmt.Errorf("load simulator machine %s: %s", path, fmt.Sprintf(format, args...))
	}
	var f overlayFile
	if err := json.Unmarshal(data, &f, json.RejectUnknownMembers(true)); err != nil {
		return fail("overlay (only extends, shared_voltage_override, description and notes are allowed): %v", err)
	}
	if f.Extends == "" {
		return fail("overlay needs a non-empty extends")
	}
	if filepath.IsAbs(f.Extends) {
		return fail("extends %q must be relative to the overlay's directory", f.Extends)
	}
	if len(f.SharedVoltageOverride) == 0 {
		return fail("overlay changes nothing: shared_voltage_override is empty")
	}
	// Identity is the real file, so a directory symlink back to an ancestor cannot make a new path each time round.
	self, err := filepath.EvalSymlinks(path)
	if err == nil {
		self, err = filepath.Abs(self)
	}
	if err != nil {
		return fail("%v", err)
	}
	if slices.Contains(chain, self) {
		return fail("extends cycle through %s", self)
	}
	parentPath := filepath.Join(filepath.Dir(path), f.Extends)
	cfg, err := loadMachine(parentPath, files, append(chain, self))
	if err != nil {
		return Config{}, fmt.Errorf("load simulator machine %s: parent: %w", path, err)
	}
	if cfg.SharedVoltage == nil {
		return fail("parent %s has no shared_voltage to override", parentPath)
	}
	type target struct {
		workload string
		core     int
	}
	seen := make(map[target]bool, len(f.SharedVoltageOverride))
	for i, o := range f.SharedVoltageOverride {
		if o.Core == nil {
			return fail("shared_voltage_override[%d] needs core", i)
		}
		if o.ThresholdV == nil && o.ThresholdClockVPer100MHz == nil {
			return fail("shared_voltage_override[%d] sets neither threshold_v nor threshold_clock_v_per_100mhz", i)
		}
		w, ok := cfg.SharedVoltage.Workload[o.Workload]
		if !ok {
			return fail("shared_voltage_override[%d] names unknown workload %q", i, o.Workload)
		}
		if *o.Core < 0 || *o.Core >= len(w.Core) {
			return fail("shared_voltage_override[%d] names unknown core %d of workload %s", i, *o.Core, o.Workload)
		}
		t := target{o.Workload, *o.Core}
		if seen[t] {
			return fail("shared_voltage_override[%d] repeats workload %s core %d", i, o.Workload, t.core)
		}
		seen[t] = true
		// The parent was just decoded, so its core slice is this load's own to edit.
		if o.ThresholdV != nil {
			w.Core[t.core].ThresholdV = *o.ThresholdV
		}
		if o.ThresholdClockVPer100MHz != nil {
			w.Core[t.core].ThresholdClockVPer100MHz = *o.ThresholdClockVPer100MHz
		}
	}
	// Facts resolve against the directory of the file that names them; rebase to this file's directory.
	if parentDir, dir := filepath.Dir(parentPath), filepath.Dir(path); cfg.Facts != "" && !filepath.IsAbs(cfg.Facts) && parentDir != dir {
		rel, err := filepath.Rel(dir, filepath.Join(parentDir, cfg.Facts))
		if err != nil {
			return fail("rebase facts %q: %v", cfg.Facts, err)
		}
		cfg.Facts = filepath.ToSlash(rel)
	}
	if _, _, err := resolve(cfg); err != nil {
		return fail("new simulator: %v", err)
	}
	return cfg, nil
}
