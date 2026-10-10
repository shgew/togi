package sim

import (
	"bytes"
	"fmt"
	"maps"
	"math"
	"reflect"
	"slices"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

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
	fmt.Fprintf(&b, "cores = %d\nfacts = %s\n", cfg.Cores, quote(cfg.Facts))
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
		fmt.Fprintf(&b, "\n[bios_context]\nbios_version = %s\nboard = %s\ncpu_model = %s\nmicrocode = %s\nboost_limit_mhz = %d\n", quote(c.BIOSVersion), quote(c.Board), quote(c.CPUModel), quote(c.Microcode), c.BoostLimitMHz)
	}
	encodeModel(&b, cfg.Model)
	if c := cfg.CCD; c != nil {
		fmt.Fprintf(&b, "\n[ccd]\nlog_rate = %s\nslope = %s\neffect = [%s, %s]\n", floatText(c.LogRate), floatText(c.Slope), floatText(c.Effect[0]), floatText(c.Effect[1]))
	}
	for core, limit := range cfg.Limits {
		fmt.Fprintf(&b, "\n[[core]]\nid = %d\nalone = %s\ntogether = %s\nflat = %s\n", core, intList(limit.Alone[:]), intList(limit.Together[:]), floatText(limit.Flat))
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
				regimes[i] = quote(string(regime))
			}
			fmt.Fprintf(&b, "regimes = [%s]\n", strings.Join(regimes, ", "))
		}
		fmt.Fprintf(&b, "rate = %s\nafter_s = %s\n", floatText(joint.Rate), decimal(joint.AfterS))
		if joint.Signal != "" {
			fmt.Fprintf(&b, "signal = %s\n", quote(string(joint.Signal)))
		}
		if joint.CrashMCECore != nil {
			fmt.Fprintf(&b, "crash_mce_core = %d\n", *joint.CrashMCECore)
		}
		members := make([]string, 0, len(joint.Members))
		for _, core := range slices.Sorted(maps.Keys(joint.Members)) {
			members = append(members, quote(strconv.Itoa(core))+" = "+strconv.Itoa(joint.Members[core]))
		}
		fmt.Fprintf(&b, "members = {%s}\n", strings.Join(members, ", "))
	}
	for _, trial := range slices.Sorted(maps.Keys(cfg.Script)) {
		s := cfg.Script[trial]
		fmt.Fprintf(&b, "\n[[script]]\ntrial = %s\n", quote(trial))
		if s.Signal != "" {
			fmt.Fprintf(&b, "signal = %s\n", quote(string(s.Signal)))
		}
		fmt.Fprintf(&b, "at_s = %s\ncore = %d\n", floatText(s.AtS), s.Core)
		if s.Reset != "" {
			fmt.Fprintf(&b, "reset = %s\n", quote(string(s.Reset)))
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
	fmt.Fprintf(b, "\n[model]\npast_limit_rate = %s\ngrowth = %s\nnear_limit_rate = %s\nonset_boost = %s\n", floatText(m.PastLimitRate), floatText(m.Growth), floatText(m.NearLimitRate), decimal(m.OnsetBoost))
	if m.CrashMCE != def.CrashMCE {
		fmt.Fprintf(b, "crash_mce = %s\n", floatText(m.CrashMCE))
	}
	if m.CoreLocalBank != def.CoreLocalBank {
		fmt.Fprintf(b, "core_local_bank = %s\n", floatText(m.CoreLocalBank))
	}
	if m.OnsetS != def.OnsetS {
		fmt.Fprintf(b, "onset_s = %s\n", floatText(m.OnsetS))
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
			fmt.Fprintf(b, "%s = %s\n", quote(string(regime)), inlineTable(m.RegimeSignals[regime], floatText))
		}
	}
}

func encodeSharedVoltage(b *bytes.Buffer, v *SharedVoltage) {
	fmt.Fprintf(b, "\n[shared_voltage]\nidle_v = %s\nmargin_v = %s\nrate = %s\nbackground_rate = %s\npower_limit_w = %s\nthermal_limit_w = %s\n", floatText(v.IdleV), floatText(v.MarginV), floatText(v.Rate), floatText(v.BackgroundRate), floatText(v.PowerLimitW), floatText(v.ThermalLimitW))
	for _, workload := range slices.Sorted(maps.Keys(v.Workload)) {
		w := v.Workload[workload]
		table := "shared_voltage.workload." + quote(workload)
		fmt.Fprintf(b, "\n[%s]\nreference_mhz = %s\nfull_mhz = [%s, %s]\nidle_gain_mhz = %s\nwatts_per_core = %s\noffset_watts_per_count = %s\npackage_mhz_per_w = %s\nbalance_mhz_per_w = %s\n", table, floatText(w.ReferenceMHz), floatText(w.FullMHz[0]), floatText(w.FullMHz[1]), floatText(w.IdleGainMHz), floatText(w.WattsPerCore), floatText(w.OffsetWattsPerCount), floatText(w.PackageMHzPerW), floatText(w.BalanceMHzPerW))
		for _, c := range w.Core {
			fmt.Fprintf(b, "\n[[%s.core]]\nbase_v = %s\nthreshold_v = %s\ncount_v = %s\nclock_v_per_100mhz = %s\nthreshold_clock_v_per_100mhz = %s\n", table, floatText(c.BaseV), floatText(c.ThresholdV), floatText(c.CountV), floatText(c.ClockVPer100MHz), floatText(c.ThresholdClockVPer100MHz))
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

// quote spells s as a TOML basic string. strconv.Quote is not TOML: it writes \a and \v, which TOML lacks. Control
// characters other than \b, \t, \n, \f and \r, and characters Go does not print, become \u escapes; everything
// else is written as strconv.Quote writes it.
func quote(s string) string {
	var b strings.Builder
	b.WriteByte('"')
	for _, r := range s {
		switch {
		case r == '"' || r == '\\':
			b.WriteByte('\\')
			b.WriteRune(r)
		case r == '\b':
			b.WriteString(`\b`)
		case r == '\t':
			b.WriteString(`\t`)
		case r == '\n':
			b.WriteString(`\n`)
		case r == '\f':
			b.WriteString(`\f`)
		case r == '\r':
			b.WriteString(`\r`)
		case r == utf8.RuneError || r >= utf8.RuneSelf && strconv.IsPrint(r):
			b.WriteRune(r)
		case r >= ' ' && r < 0x7f:
			b.WriteRune(r)
		case r <= 0xffff:
			fmt.Fprintf(&b, `\u%04x`, r)
		default:
			fmt.Fprintf(&b, `\U%08x`, r)
		}
	}
	b.WriteByte('"')
	return b.String()
}

// floatText spells x as a TOML float token: 17 significant digits, TOML's lowercase inf and nan, and a ".0" on a
// whole number beyond ±(2^53-1), where the decoder rejects an integer token for a float.
func floatText(x float64) string {
	switch {
	case math.IsNaN(x):
		return "nan"
	case math.IsInf(x, 1):
		return "inf"
	case math.IsInf(x, -1):
		return "-inf"
	}
	s := strconv.FormatFloat(x, 'g', 17, 64)
	if !strings.ContainsAny(s, ".e") && math.Abs(x) > 1<<53-1 {
		s += ".0"
	}
	return s
}

// decimal spells x as TOML float with a decimal point even when it is whole, as the committed machine files do
// for onset_boost and after_s.
func decimal(x float64) string {
	s := floatText(x)
	if !strings.ContainsAny(s, ".en") {
		s += ".0"
	}
	return s
}

func inlineTable[K ~string, V any](m map[K]V, format func(V) string) string {
	parts := make([]string, 0, len(m))
	for _, key := range slices.Sorted(maps.Keys(m)) {
		parts = append(parts, quote(string(key))+" = "+format(m[key]))
	}
	return "{" + strings.Join(parts, ", ") + "}"
}
