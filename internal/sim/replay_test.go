package sim

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestReplayDraws(t *testing.T) {
	bios := defaultBIOSContext
	other := bios
	other.BIOSVersion = "other"
	class := journal.TrialClass{Regime: machine.R7, Workload: "work", Cores: []int{1, 0}, DurationS: 60}
	profile := []int{-20, -21}
	replay, err := NewReplay(bios, []ReplayFact{
		{Context: bios, Class: class, Profile: profile, Outcome: journal.OutcomePass, DurationS: 60},
		{Context: bios, Class: class, Profile: profile, Outcome: journal.OutcomeFailure, Signal: machine.Stall, DurationS: 7},
		{Context: bios, Class: class, Profile: profile, Outcome: journal.OutcomeFailure, Signal: machine.UnexpectedExit, DurationS: 9},
		{Context: other, Class: class, Profile: profile, Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, DurationS: 11},
		{Context: bios, Class: class, Profile: []int{-21, -21}, Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, DurationS: 13},
	})
	if err != nil {
		t.Fatal(err)
	}
	type answer struct {
		Signal machine.Signal
		Ran    time.Duration
	}
	seen := make(map[answer]bool)
	for seed := uint64(1); seed <= 64; seed++ {
		cfg := Config{Seed: seed, Cores: 2, BIOSContext: bios, BIOS: profile, Model: sharp(machine.ComputationError), Limits: flat(2, -10, -10), Replay: replay}
		var answers []answer
		for range 2 {
			m := newMachine(t, cfg)
			run, err := m.Seams().Trials.Start(context.Background(), machine.TrialSpec{ID: "1", Index: 4, Regime: machine.R7, Workload: machine.Workload{ID: "work"}, Condition: machine.Parked, Cores: []int{0, 1}, Duration: time.Minute})
			if err != nil {
				t.Fatal(err)
			}
			res, err := run.Wait(context.Background(), nil)
			if err != nil {
				t.Fatal(err)
			}
			answers = append(answers, answer{res.Signal, res.Ran})
		}
		if diff := cmp.Diff(answers[0], answers[1]); diff != "" {
			t.Fatal(diff)
		}
		seen[answers[0]] = true
	}
	want := map[answer]bool{{"", time.Minute}: true, {machine.Stall, 7 * time.Second}: true, {machine.UnexpectedExit, 9 * time.Second}: true}
	if diff := cmp.Diff(want, seen); diff != "" {
		t.Fatal(diff)
	}
}

func TestReplayFallback(t *testing.T) {
	bios := defaultBIOSContext
	class := journal.TrialClass{Regime: machine.R1, Workload: "work", Cores: []int{0}, DurationS: 60}
	replay, err := NewReplay(bios, []ReplayFact{{Context: bios, Class: class, Profile: []int{-20, -21}, Outcome: journal.OutcomePass, DurationS: 60}})
	if err != nil {
		t.Fatal(err)
	}
	base := machine.TrialSpec{ID: "1", Regime: machine.R1, Workload: machine.Workload{ID: "work"}, Cores: []int{0}, Duration: time.Minute}
	for _, name := range []string{"regime", "workload", "cores", "duration", "loaded offset", "unloaded offset", "context", "idle"} {
		t.Run(name, func(t *testing.T) {
			cfg := Config{Seed: 42, Cores: 2, BIOSContext: bios, BIOS: []int{-20, -21}, Limits: flat(2, -10, -10), Model: sharp(machine.ComputationError)}
			spec := base
			switch name {
			case "regime":
				spec.Regime = machine.R2
			case "workload":
				spec.Workload.ID = "other"
			case "cores":
				spec.Cores = []int{1}
			case "duration":
				spec.Duration = 90 * time.Second
			case "loaded offset":
				cfg.BIOS[0]--
			case "unloaded offset":
				cfg.BIOS[1]--
			case "context":
				cfg.BIOSContext.BIOSVersion = "other"
			case "idle":
				spec.Cores = nil
			}
			var results []machine.Result
			var crashes []bool
			for _, oracle := range []*Replay{nil, replay} {
				cfg.Replay = oracle
				m := newMachine(t, cfg)
				if diff := cmp.Diff(false, m.HasRealAnswer(cfg.BIOS, spec)); diff != "" {
					t.Fatal(diff)
				}
				run, err := m.Seams().Trials.Start(context.Background(), spec)
				if err != nil {
					t.Fatal(err)
				}
				res, err := run.Wait(context.Background(), nil)
				if err != nil && !errors.Is(err, machine.ErrCrashed) {
					t.Fatal(err)
				}
				results = append(results, res)
				crashes = append(crashes, errors.Is(err, machine.ErrCrashed))
			}
			if diff := cmp.Diff(results[0], results[1]); diff != "" {
				t.Fatal(diff)
			}
			if diff := cmp.Diff(crashes[0], crashes[1]); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestReplayCrashTime(t *testing.T) {
	bios := defaultBIOSContext
	for _, signal := range []machine.Signal{machine.Crash, machine.UncorrectedMCE} {
		t.Run(string(signal), func(t *testing.T) {
			replay, err := NewReplay(bios, []ReplayFact{{Context: bios, Class: journal.TrialClass{Regime: machine.R7, Workload: "work", Cores: []int{0, 1}, DurationS: 60}, Profile: []int{-20, -21}, Outcome: journal.OutcomeFailure, Signal: signal, DurationS: 7}})
			if err != nil {
				t.Fatal(err)
			}
			m := newMachine(t, Config{Seed: 3, Cores: 2, BIOSContext: bios, BIOS: []int{-20, -21}, Limits: flat(2, -50, -50), Replay: replay})
			spec := machine.TrialSpec{Regime: machine.R7, Workload: machine.Workload{ID: "work"}, Cores: []int{0, 1}, Duration: time.Minute}
			if diff := cmp.Diff(true, m.HasRealAnswer([]int{-20, -21}, spec)); diff != "" {
				t.Fatal(diff)
			}
			progress := &progressRecorder{}
			signals := &signalRecorder{}
			var report machine.Reporter = progress
			if signal == machine.UncorrectedMCE {
				report = signals
			}
			_, err = runSpec(t, m, "1", spec.Regime, spec.Workload, spec.Cores, spec.Duration, report)
			if signal == machine.Crash {
				if diff := cmp.Diff([]string{"simulated replayed crash at recorded exposure"}, progress.details); diff != "" {
					t.Fatal(diff)
				}
			} else {
				if diff := cmp.Diff([]machine.Signal{machine.UncorrectedMCE}, signals.signals); diff != "" {
					t.Fatal(diff)
				}
			}
			if diff := cmp.Diff(true, errors.Is(err, machine.ErrCrashed)); diff != "" {
				t.Fatal(diff)
			}
			if diff := cmp.Diff(7*time.Second, m.Monotonic()); diff != "" {
				t.Fatal(diff)
			}
			m.Reboot()
			boot, err := m.Seams().Host.BootID()
			if err != nil {
				t.Fatal(err)
			}
			mces, err := m.Seams().Kernel.MCEs(boot, 0)
			if err != nil {
				t.Fatal(err)
			}
			want := 0
			if signal == machine.UncorrectedMCE {
				want = 1
			}
			if diff := cmp.Diff(want, len(mces)); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestReplayRequiresDeclaredContext(t *testing.T) {
	_, err := NewReplay(machine.BIOSContext{}, nil)
	if diff := cmp.Diff(true, err != nil); diff != "" {
		t.Fatal(diff)
	}
}

func TestReplayCorrectedMCETime(t *testing.T) {
	bios := defaultBIOSContext
	replay, err := NewReplay(bios, []ReplayFact{{Context: bios, Class: journal.TrialClass{Regime: machine.R1, Workload: "work", Cores: []int{0}, DurationS: 60}, Profile: []int{-20, 0}, Outcome: journal.OutcomeFailure, Signal: machine.CorrectedMCE, DurationS: 7}})
	if err != nil {
		t.Fatal(err)
	}
	m := newMachine(t, Config{Cores: 2, BIOSContext: bios, BIOS: []int{-20, 0}, Replay: replay})
	res, err := runSpec(t, m, "1", machine.R1, machine.Workload{ID: "work"}, []int{0}, time.Minute, nil)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(7*time.Second, res.Ran); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff(7*time.Second, m.Monotonic()); diff != "" {
		t.Fatal(diff)
	}
	boot, err := m.Seams().Host.BootID()
	if err != nil {
		t.Fatal(err)
	}
	mces, err := m.Seams().Kernel.MCEs(boot, 0)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(1, len(mces)); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff(true, mces[0].Corrected); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff(7*time.Second, mces[0].Monotonic); diff != "" {
		t.Fatal(diff)
	}
}

func TestReplayRejectsInvalidEvidence(t *testing.T) {
	base := ReplayFact{Context: defaultBIOSContext, Class: journal.TrialClass{Regime: machine.R1, Workload: "work", Cores: []int{0}, DurationS: 60}, Profile: []int{-10, 0}, Outcome: journal.OutcomeFailure, Signal: machine.Stall, DurationS: 7}
	for _, name := range []string{"no cores", "no duration", "negative exposure", "excess exposure", "unknown signal", "negative core", "outside core", "duplicate core"} {
		t.Run(name, func(t *testing.T) {
			f := base
			switch name {
			case "no cores":
				f.Class.Cores = nil
			case "no duration":
				f.Class.DurationS = 0
			case "negative exposure":
				f.DurationS = -1
			case "excess exposure":
				f.DurationS = 61
			case "unknown signal":
				f.Signal = "invalid"
			case "negative core":
				f.Class.Cores = []int{-1}
			case "outside core":
				f.Class.Cores = []int{2}
			case "duplicate core":
				f.Class.Cores = []int{0, 0}
			}
			if _, err := NewReplay(defaultBIOSContext, []ReplayFact{f}); err == nil || !strings.Contains(err.Error(), "fact 0") {
				t.Fatalf("invalid evidence = %v", err)
			}
		})
	}
}
