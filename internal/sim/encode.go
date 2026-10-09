package sim

import (
	"bytes"
	"fmt"
	"maps"
	"reflect"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/shgew/togi/internal/machine"
)

// EncodeMachine returns cfg as a machine file that LoadMachine reads back as an equal config. It writes every
// machine-file field and returns an error for a field the file cannot hold: Seed, Replay, Boots and Start. A model
// scalar or map the file leaves to DefaultModel is written only where cfg differs from it, apart from the
// three fitted hazard parameters and onset_boost, which are always written.
func EncodeMachine(cfg Config) ([]byte, error) {
	var runtimeOnly []string
	if cfg.Seed != 0 {
		runtimeOnly = append(runtimeOnly, "Seed")
	}
	if cfg.Replay != nil {
		runtimeOnly = append(runtimeOnly, "Replay")
	}
	if cfg.Boots != 0 {
		runtimeOnly = append(runtimeOnly, "Boots")
	}
	if !cfg.Start.Equal(time.Time{}) {
		runtimeOnly = append(runtimeOnly, "Start")
	}
	if len(runtimeOnly) > 0 {
		return nil, fmt.Errorf("encode simulator machine: %s is runtime-only, not a machine-file field", strings.Join(runtimeOnly, ", "))
	}
	var b bytes.Buffer
	fmt.Fprintf(&b, "cores = %d\nfacts = %s\n", cfg.Cores, strconv.Quote(cfg.Facts))
	if cfg.BIOS != nil {
		fmt.Fprintf(&b, "bios = %s\n", intList(cfg.BIOS))
	}
	if cfg.Ranking != nil {
		fmt.Fprintf(&b, "ranking = %s\n", intList(cfg.Ranking))
	}
	if cfg.OldKernel {
		fmt.Fprintln(&b, "old_kernel = true")
	}
	if c := cfg.BIOSContext; c != (machine.BIOSContext{}) {
		fmt.Fprintf(&b, "\n[bios_context]\nbios_version = %s\nboard = %s\ncpu_model = %s\nmicrocode = %s\nboost_limit_mhz = %d\n", strconv.Quote(c.BIOSVersion), strconv.Quote(c.Board), strconv.Quote(c.CPUModel), strconv.Quote(c.Microcode), c.BoostLimitMHz)
	}
	encodeModel(&b, cfg.Model)
	if c := cfg.CCD; c != nil {
		fmt.Fprintf(&b, "\n[ccd]\nlog_rate = %.17g\nslope = %.17g\neffect = [%.17g, %.17g]\n", c.LogRate, c.Slope, c.Effect[0], c.Effect[1])
	}
	for core, limit := range cfg.Limits {
		fmt.Fprintf(&b, "\n[[core]]\nid = %d\nalone = %s\ntogether = %s\nflat = %.17g\n", core, intList(limit.Alone[:]), intList(limit.Together[:]), limit.Flat)
		if limit.Idle != nil {
			fmt.Fprintf(&b, "idle = %d\n", *limit.Idle)
		}
		if len(limit.Workload) > 0 {
			fmt.Fprintf(&b, "workload = %s\n", inlineTable(limit.Workload, strconv.Itoa))
		}
	}
	for _, joint := range cfg.Joints {
		fmt.Fprint(&b, "\n[[joint]]\n")
		if joint.Regimes != nil {
			regimes := make([]string, len(joint.Regimes))
			for i, regime := range joint.Regimes {
				regimes[i] = strconv.Quote(string(regime))
			}
			fmt.Fprintf(&b, "regimes = [%s]\n", strings.Join(regimes, ", "))
		}
		fmt.Fprintf(&b, "rate = %.17g\nafter_s = %s\n", joint.Rate, decimal(joint.AfterS))
		if joint.Signal != "" {
			fmt.Fprintf(&b, "signal = %s\n", strconv.Quote(string(joint.Signal)))
		}
		if joint.CrashMCECore != nil {
			fmt.Fprintf(&b, "crash_mce_core = %d\n", *joint.CrashMCECore)
		}
		members := make([]string, 0, len(joint.Members))
		for _, core := range slices.Sorted(maps.Keys(joint.Members)) {
			members = append(members, strconv.Quote(strconv.Itoa(core))+" = "+strconv.Itoa(joint.Members[core]))
		}
		fmt.Fprintf(&b, "members = {%s}\n", strings.Join(members, ", "))
	}
	for _, trial := range slices.Sorted(maps.Keys(cfg.Script)) {
		s := cfg.Script[trial]
		fmt.Fprintf(&b, "\n[[script]]\ntrial = %s\n", strconv.Quote(trial))
		if s.Signal != "" {
			fmt.Fprintf(&b, "signal = %s\n", strconv.Quote(string(s.Signal)))
		}
		fmt.Fprintf(&b, "at_s = %.17g\ncore = %d\n", s.AtS, s.Core)
		if s.Reset != "" {
			fmt.Fprintf(&b, "reset = %s\n", strconv.Quote(string(s.Reset)))
		}
		if s.ThenCrash {
			fmt.Fprintln(&b, "then_crash = true")
		}
	}
	if v := cfg.SharedVoltage; v != nil {
		encodeSharedVoltage(&b, v)
	}
	return b.Bytes(), nil
}

func encodeModel(b *bytes.Buffer, m *Model) {
	def := DefaultModel()
	if m == nil {
		m = &def
	}
	fmt.Fprintf(b, "\n[model]\npast_limit_rate = %.17g\ngrowth = %.17g\nnear_limit_rate = %.17g\nonset_boost = %s\n", m.PastLimitRate, m.Growth, m.NearLimitRate, decimal(m.OnsetBoost))
	if m.CrashMCE != def.CrashMCE {
		fmt.Fprintf(b, "crash_mce = %.17g\n", m.CrashMCE)
	}
	if m.CoreLocalBank != def.CoreLocalBank {
		fmt.Fprintf(b, "core_local_bank = %.17g\n", m.CoreLocalBank)
	}
	if m.OnsetS != def.OnsetS {
		fmt.Fprintf(b, "onset_s = %.17g\n", m.OnsetS)
	}
	if m.RegimeSignals != nil || !reflect.DeepEqual(m.Signals, def.Signals) {
		fmt.Fprintf(b, "signals = %s\n", inlineTable(m.Signals, floatText))
	}
	if !reflect.DeepEqual(m.Reset, def.Reset) {
		fmt.Fprintf(b, "reset = %s\n", inlineTable(m.Reset, floatText))
	}
	if m.RegimeSignals != nil {
		fmt.Fprint(b, "\n[model.regime_signals]\n")
		for _, regime := range slices.Sorted(maps.Keys(m.RegimeSignals)) {
			fmt.Fprintf(b, "%s = %s\n", strconv.Quote(string(regime)), inlineTable(m.RegimeSignals[regime], floatText))
		}
	}
}

func encodeSharedVoltage(b *bytes.Buffer, v *SharedVoltage) {
	fmt.Fprintf(b, "\n[shared_voltage]\nidle_v = %.17g\nmargin_v = %.17g\nrate = %.17g\nbackground_rate = %.17g\npower_limit_w = %.17g\nthermal_limit_w = %.17g\n", v.IdleV, v.MarginV, v.Rate, v.BackgroundRate, v.PowerLimitW, v.ThermalLimitW)
	for _, workload := range slices.Sorted(maps.Keys(v.Workload)) {
		w := v.Workload[workload]
		table := "shared_voltage.workload." + strconv.Quote(workload)
		fmt.Fprintf(b, "\n[%s]\nreference_mhz = %.17g\nfull_mhz = [%.17g, %.17g]\nidle_gain_mhz = %.17g\nwatts_per_core = %.17g\noffset_watts_per_count = %.17g\npackage_mhz_per_w = %.17g\nbalance_mhz_per_w = %.17g\n", table, w.ReferenceMHz, w.FullMHz[0], w.FullMHz[1], w.IdleGainMHz, w.WattsPerCore, w.OffsetWattsPerCount, w.PackageMHzPerW, w.BalanceMHzPerW)
		for _, c := range w.Core {
			fmt.Fprintf(b, "\n[[%s.core]]\nbase_v = %.17g\nthreshold_v = %.17g\ncount_v = %.17g\nclock_v_per_100mhz = %.17g\nthreshold_clock_v_per_100mhz = %.17g\n", table, c.BaseV, c.ThresholdV, c.CountV, c.ClockVPer100MHz, c.ThresholdClockVPer100MHz)
			if c.Signals != nil {
				fmt.Fprintf(b, "signals = %s\n", inlineTable(c.Signals, floatText))
			}
		}
	}
}

func intList(values []int) string {
	parts := make([]string, len(values))
	for i, v := range values {
		parts[i] = strconv.Itoa(v)
	}
	return "[" + strings.Join(parts, ", ") + "]"
}

func floatText(x float64) string { return strconv.FormatFloat(x, 'g', 17, 64) }

// decimal spells x as TOML float with a decimal point even when it is whole, as the committed machine files do
// for onset_boost and after_s.
func decimal(x float64) string {
	s := fmt.Sprintf("%.17g", x)
	if !strings.ContainsAny(s, ".eEnN") {
		s += ".0"
	}
	return s
}

func inlineTable[K ~string, V any](m map[K]V, format func(V) string) string {
	parts := make([]string, 0, len(m))
	for _, key := range slices.Sorted(maps.Keys(m)) {
		parts = append(parts, strconv.Quote(string(key))+" = "+format(m[key]))
	}
	return "{" + strings.Join(parts, ", ") + "}"
}
