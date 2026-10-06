package sim

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func TestLoadMachine(t *testing.T) {
	path := filepath.Join(t.TempDir(), "machine.toml")
	content := `cores = 2
bios = [0, -1]
ranking = [9, 7]
old_kernel = true
[model]
past_limit_rate = 0.7
onset_boost = 2
[model.signals]
crash = 1
[model.reset]
thermal_trip = 1
[[core]]
id = 0
alone = [-30, -30, -30, -30, -30]
together = [-29, -29, -29, -29, -29, -29, -29]
idle = -45
workload = { "special" = -27 }
flat = 0.1
[[core]]
id = 1
alone = [-30, -30, -30, -30, -30]
together = [-29, -29, -29, -29, -29, -29, -29]
[[joint]]
members = { "0" = -20, "1" = -20 }
regimes = ["R7"]
rate = 0.05
after_s = 30
signal = "crash"
[[script]]
trial = "0042"
signal = "crash"
at_s = 3
core = 1
reset = "watchdog"
then_crash = true
`
	if err := os.WriteFile(path, []byte(content), 0600); err != nil {
		t.Fatal(err)
	}
	cfg, err := LoadMachine(path)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]int{9, 7}, cfg.Ranking); diff != "" {
		t.Fatal(diff)
	}
	if cfg.Model.PastLimitRate != .7 || cfg.Model.OnsetS != 100 || cfg.Model.Reset[machine.ResetThermalTrip] != 1 || cfg.Limits[0].Workload["special"] != -27 || cfg.Joints[0].AfterS != 30 || !cfg.Script["0042"].ThenCrash {
		t.Fatalf("incomplete config: %+v", cfg)
	}
	if err := os.WriteFile(path, []byte(content+"unknown = 1\n"), 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadMachine(path); err == nil || !strings.Contains(err.Error(), path) || !strings.Contains(err.Error(), "unknown") {
		t.Fatalf("unknown key error: %v", err)
	}
}

func TestLoadMachineInvalidDefinitions(t *testing.T) {
	core := "[[core]]\nid = %d\nalone = [-10, -10, -10, -10, -10]\ntogether = [-10, -10, -10, -10, -10, -10, -10]\n"
	for _, tc := range []struct{ name, content, want string }{
		{"syntax", "cores = [", "load simulator machine"},
		{"negative core", "cores = 2\n" + fmt.Sprintf(core, -1), "invalid or duplicate core -1"},
		{"outside core", "cores = 2\n" + fmt.Sprintf(core, 2), "invalid or duplicate core 2"},
		{"duplicate core", "cores = 2\n" + fmt.Sprintf(core, 0) + fmt.Sprintf(core, 0), "invalid or duplicate core 0"},
		{"limit shape", "cores = 2\n[[core]]\nid = 0\nalone = [-10]\n", "needs five alone and seven together limits"},
		{"missing core", "cores = 2\n" + fmt.Sprintf(core, 0), "missing core 1"},
		{"default topology incomplete", fmt.Sprintf(core, 0), "missing core 1"},
		{"joint identity", "cores = 2\n[[joint]]\nmembers = { nope = -10 }\n", "joint member \"nope\""},
		{"empty script", "cores = 2\n[[script]]\ntrial = \"\"\n", "empty script trial"},
		{"duplicate script", "cores = 2\n[[script]]\ntrial = \"0001\"\n[[script]]\ntrial = \"0001\"\n", "duplicate script trial 0001"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "machine.toml")
			if err := os.WriteFile(path, []byte(tc.content), 0600); err != nil {
				t.Fatal(err)
			}
			_, err := LoadMachine(path)
			if err == nil || !strings.Contains(err.Error(), tc.want) || !strings.Contains(err.Error(), path) {
				t.Fatalf("load error = %v, want path and %q", err, tc.want)
			}
		})
	}
}

func TestLoadMachineRejectsInvalidCoreCountBeforeLimits(t *testing.T) {
	t.Parallel()
	path := filepath.Join(t.TempDir(), "machine.toml")
	if err := os.WriteFile(path, []byte("cores = -2\n[[core]]\nid = 0\n"), 0600); err != nil {
		t.Fatal(err)
	}
	_, err := LoadMachine(path)
	want := fmt.Sprintf("load simulator machine %s: new simulator: -2 cores: must be even and at least 2", path)
	if err == nil || err.Error() != want {
		t.Fatalf("got %v, want %s", err, want)
	}
}

func TestLoadMachineBIOSContext(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		content string
		want    machine.BIOSContext
	}{
		{"unset", "cores = 2\n", machine.BIOSContext{}},
		{"present", "cores = 2\n[bios_context]\nbios_version = \"B.2\"\nboard = \"X670\"\ncpu_model = \"Zen 5\"\nmicrocode = \"0x123\"\nboost_limit_mhz = 5900\n", machine.BIOSContext{BIOSVersion: "B.2", Board: "X670", CPUModel: "Zen 5", Microcode: "0x123", BoostLimitMHz: 5900}},
		{"partial", "cores = 2\n[bios_context]\nbios_version = \"SIM.2\"\n", machine.BIOSContext{BIOSVersion: "SIM.2", Board: defaultBIOSContext.Board, CPUModel: defaultBIOSContext.CPUModel, Microcode: defaultBIOSContext.Microcode, BoostLimitMHz: defaultBIOSContext.BoostLimitMHz}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			path := filepath.Join(t.TempDir(), "machine.toml")
			if err := os.WriteFile(path, []byte(tc.content), 0600); err != nil {
				t.Fatal(err)
			}
			cfg, err := LoadMachine(path)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.want, cfg.BIOSContext); diff != "" {
				t.Fatalf("BIOS context (-want +got): %s", diff)
			}
			m := newMachine(t, cfg)
			context, err := m.Seams().Host.BIOSContext()
			if err != nil {
				t.Fatal(err)
			}
			expected := tc.want
			if expected == (machine.BIOSContext{}) {
				expected = defaultBIOSContext
			}
			if diff := cmp.Diff(expected, context); diff != "" {
				t.Fatalf("host BIOS context (-want +got): %s", diff)
			}
		})
	}
}

func TestNewRejectsInvalidScriptCore(t *testing.T) {
	t.Parallel()
	for _, core := range []int{-1, 2} {
		_, err := New(Config{Cores: 2, Script: map[string]Outcome{"0001": {Signal: machine.Crash, Core: core}}})
		want := fmt.Sprintf("new simulator: script trial 0001 core %d outside [0, 2)", core)
		if err == nil || err.Error() != want {
			t.Fatalf("core %d: got %v, want %s", core, err, want)
		}
	}
}

func TestNewKeepsItsOwnConfig(t *testing.T) {
	t.Parallel()
	cfg := Config{Cores: 2, Limits: flat(2, -20, -20), Script: map[string]Outcome{"0001": {Signal: machine.Stall, AtS: 1}}}
	m, err := New(cfg)
	if err != nil {
		t.Fatal(err)
	}
	cfg.Limits[0].Alone = [5]int{-5, -5, -5, -5, -5}
	delete(cfg.Script, "0001")
	if got := m.AloneLimit(0); got != -20 {
		t.Fatalf("alone limit %d after the caller changed its config, want -20", got)
	}
	res, err := runSpec(t, m, "0001", machine.R1, machine.PickWorkload(machine.R1, 0), []int{0}, time.Minute, nil)
	if err != nil || res.Signal != machine.Stall || res.Ran != time.Second {
		t.Fatalf("trial 0001: %+v, %v; want the script given to New", res, err)
	}
}

func TestScriptTrialAfterNew(t *testing.T) {
	t.Parallel()
	m, err := New(Config{Cores: 2, Limits: flat(2, -20, -20)})
	if err != nil {
		t.Fatal(err)
	}
	if err := m.ScriptTrial("0001", Outcome{Signal: machine.ComputationError, AtS: 2, Core: 1}); err != nil {
		t.Fatal(err)
	}
	res, err := runSpec(t, m, "0001", machine.R1, machine.PickWorkload(machine.R1, 0), []int{1}, time.Minute, nil)
	if err != nil || res.Signal != machine.ComputationError || res.Core != 1 || res.Ran != 2*time.Second {
		t.Fatalf("trial 0001: %+v, %v; want the scripted computation error", res, err)
	}
	want := "script simulator trial: script trial 0002 core 2 outside [0, 2)"
	if err := m.ScriptTrial("0002", Outcome{Signal: machine.Crash, Core: 2}); err == nil || err.Error() != want {
		t.Fatalf("got %v, want %s", err, want)
	}
}

func TestNewRejectsInvalidModelWeights(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		signals map[machine.Signal]float64
		reset   map[machine.ResetKind]float64
		want    string
	}{
		{"signal unknown sorted", map[machine.Signal]float64{"z": 1, "a": 1}, nil, `new simulator: signal "a" is not supported`},
		{"signal negative sorted", map[machine.Signal]float64{machine.Stall: -1, machine.ComputationError: -2}, nil, `new simulator: signal "computation_error" weight -2 is negative`},
		{"reset unknown sorted", nil, map[machine.ResetKind]float64{"z": 1, "a": 1}, `new simulator: reset "a" is not supported`},
		{"reset negative sorted", nil, map[machine.ResetKind]float64{machine.ResetWatchdog: -1, machine.ResetThermalTrip: -2}, `new simulator: reset "thermal_trip" weight -2 is negative`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			model := DefaultModel()
			if tc.signals != nil {
				model.Signals = tc.signals
			}
			if tc.reset != nil {
				model.Reset = tc.reset
			}
			_, err := New(Config{Cores: 2, Model: &model})
			if err == nil || err.Error() != tc.want {
				t.Fatalf("got %v, want %s", err, tc.want)
			}
		})
	}
}

func TestNewRejectsInvalidHazardsAndSignals(t *testing.T) {
	t.Parallel()
	negative := flat(2, -10, -10)
	negative[1].Flat = -1
	for _, tc := range []struct {
		name string
		cfg  Config
		want string
	}{
		{"negative flat", Config{Cores: 2, Limits: negative}, "new simulator: flat rate -1 of core 1 is negative"},
		{"script signal", Config{Cores: 2, Script: map[string]Outcome{"0001": {Signal: "crsh"}}}, `new simulator: script trial 0001 signal "crsh" is not supported`},
		{"script reset", Config{Cores: 2, Script: map[string]Outcome{"0001": {Reset: "brownout"}}}, `new simulator: script trial 0001 reset "brownout" is not supported`},
		{"joint regime", Config{Cores: 2, Joints: []Joint{{Members: map[int]int{0: -5}, Regimes: []machine.Regime{"R9"}}}}, `new simulator: joint regime "R9" is not supported`},
		{"joint signal", Config{Cores: 2, Joints: []Joint{{Members: map[int]int{0: -5}, Signal: "crsh"}}}, `new simulator: joint signal "crsh" is not supported`},
		{"joint rate", Config{Cores: 2, Joints: []Joint{{Members: map[int]int{0: -5}, Rate: -1}}}, "new simulator: joint rate -1 or delay 0 is negative"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			if _, err := New(tc.cfg); err == nil || err.Error() != tc.want {
				t.Fatalf("got %v, want %s", err, tc.want)
			}
		})
	}
}

func TestNewRejectsInvalidModelTopology(t *testing.T) {
	for _, tc := range []struct {
		name string
		cfg  Config
		want string
	}{
		{"ranking", Config{Cores: 2, Ranking: []int{1}}, "1 ranking values for 2 cores"},
		{"idle below floor", Config{Cores: 2, Limits: []Limits{{Idle: new(-51)}, {}}}, "idle limit -51 of core 0 outside [-50, 1]"},
		{"idle above zero failure", Config{Cores: 2, Limits: []Limits{{Idle: new(2)}, {}}}, "idle limit 2 of core 0 outside [-50, 1]"},
		{"workload below floor", Config{Cores: 2, Limits: []Limits{{Workload: map[string]int{"custom": -51}}, {}}}, "workload custom limit -51"},
		{"workload above zero failure", Config{Cores: 2, Limits: []Limits{{Workload: map[string]int{"custom": 2}}, {}}}, "workload custom limit 2"},
		{"joint core negative", Config{Cores: 2, Joints: []Joint{{Members: map[int]int{-1: -10}}}}, "joint member core -1 offset -10 invalid"},
		{"joint core outside", Config{Cores: 2, Joints: []Joint{{Members: map[int]int{2: -10}}}}, "joint member core 2 offset -10 invalid"},
		{"joint offset", Config{Cores: 2, Joints: []Joint{{Members: map[int]int{0: 1}}}}, "joint member core 0 offset 1 invalid"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			_, err := New(tc.cfg)
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("New error = %v, want %q", err, tc.want)
			}
		})
	}
}
