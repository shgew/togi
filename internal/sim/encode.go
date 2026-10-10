package sim

import (
	"fmt"
	"maps"
	"reflect"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/shgew/togi/internal/machine"
)

// EncodeMachine returns cfg as a JSON machine file that LoadMachine reads back as an equal config.
// Optional notes carry generated provenance without changing the simulated machine. Seed, Replay, Boots
// and Start are runtime-only and cannot be represented in a machine file.
func EncodeMachine(cfg Config, notes ...string) ([]byte, error) {
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
	f := machineFile{Notes: notes, Cores: cfg.Cores, Facts: cfg.Facts, BIOS: cfg.BIOS, Ranking: cfg.Ranking, OldKernel: cfg.OldKernel, CCD: cfg.CCD, SharedVoltage: cfg.SharedVoltage}
	if cfg.BIOSContext != (machine.BIOSContext{}) {
		f.BIOSContext = &cfg.BIOSContext
	}
	def := DefaultModel()
	m := cfg.Model
	if m == nil {
		m = &def
	}
	f.Model = fileModel{PastLimitRate: &m.PastLimitRate, Growth: &m.Growth, NearLimitRate: &m.NearLimitRate, OnsetBoost: &m.OnsetBoost}
	if m.CrashMCE != def.CrashMCE {
		f.Model.CrashMCE = &m.CrashMCE
	}
	if m.CoreLocalBank != def.CoreLocalBank {
		f.Model.CoreLocalBank = &m.CoreLocalBank
	}
	if m.OnsetS != def.OnsetS {
		f.Model.OnsetS = &m.OnsetS
	}
	if m.RegimeSignals != nil || !reflect.DeepEqual(m.Signals, def.Signals) {
		f.Model.Signals = m.Signals
	}
	if !reflect.DeepEqual(m.Reset, def.Reset) {
		f.Model.Reset = m.Reset
	}
	f.Model.RegimeSignals = m.RegimeSignals
	if len(cfg.Limits) > 0 {
		f.Core = make([]fileCore, len(cfg.Limits))
	}
	for core := range cfg.Limits {
		limit := &cfg.Limits[core]
		f.Core[core] = fileCore{ID: core, Alone: limit.Alone[:], Together: limit.Together[:], Flat: limit.Flat, Idle: limit.Idle, Workload: limit.Workload}
	}
	if len(cfg.Joints) > 0 {
		f.Joint = make([]fileJoint, len(cfg.Joints))
	}
	for i, joint := range cfg.Joints {
		members := make(map[string]int, len(joint.Members))
		for core, offset := range joint.Members {
			members[strconv.Itoa(core)] = offset
		}
		f.Joint[i] = fileJoint{Regimes: joint.Regimes, Rate: joint.Rate, AfterS: joint.AfterS, Members: members, Signal: joint.Signal, CrashMCECore: joint.CrashMCECore}
	}
	for _, trial := range slices.Sorted(maps.Keys(cfg.Script)) {
		s := cfg.Script[trial]
		f.Script = append(f.Script, fileScript{Trial: trial, Signal: s.Signal, AtS: s.AtS, Core: s.Core, Reset: s.Reset, ThenCrash: s.ThenCrash})
	}
	return f.encode()
}
