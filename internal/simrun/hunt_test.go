package simrun

import (
	"context"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"slices"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/tuner"
)

func huntConfig(cores int) sim.Config {
	model := sim.DefaultModel()
	model.PastLimitRate = 1
	model.Signals = map[machine.Signal]float64{machine.Crash: 1}
	model.CrashMCE = 0
	limits := make([]sim.Limits, cores)
	for i := range limits {
		limits[i].Alone = [5]int{-10, -10, -10, -10, -10}
		limits[i].Together = [7]int{-10, -10, -10, -10, -10, -10, -50}
	}
	return sim.Config{Seed: 1, Cores: cores, Limits: limits, Model: &model}
}

func runHunt(t *testing.T, cfg sim.Config, setup func(*sim.Machine), tweak func(*Input)) (session.Stop, []journal.Event, string) {
	t.Helper()
	m, err := sim.New(cfg)
	if err != nil {
		t.Fatal(err)
	}
	if setup != nil {
		setup(m)
	}
	dir := t.TempDir()
	in := Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Cycles: 1, InMemoryJournal: true}
	if tweak != nil {
		tweak(&in)
	}
	stop, err := Simulate(context.Background(), in)
	if err != nil {
		t.Fatal(err)
	}
	events, torn, err := journal.Read(dir)
	if err != nil || torn != nil {
		t.Fatalf("read events: %v, torn %q", err, torn)
	}
	return stop, events, dir
}

func stopAfterAlonePasses(in *Input, cores int) {
	trials := map[string]int{}
	passed := map[int]bool{}
	in.Until = func(e journal.Event) bool {
		switch p := e.Data.(type) {
		case *journal.TrialIntent:
			if p.Condition == machine.Alone && p.Core != nil {
				trials[p.Trial] = *p.Core
			}
		case *journal.TrialEnd:
			if p.Outcome == journal.OutcomePass {
				if core, ok := trials[p.Trial]; ok {
					passed[core] = true
				}
			}
		}
		return len(passed) == cores
	}
}

func findPayload[P journal.Payload](events []journal.Event, accept func(P) bool) (P, bool) {
	var zero P
	for _, e := range events {
		p, ok := e.Data.(P)
		if ok && (accept == nil || accept(p)) {
			return p, true
		}
	}
	return zero, false
}

func assertAdversarialEvidence(t *testing.T, events []journal.Event, huntBudget, crashBudget int) []int {
	t.Helper()
	s := tuner.New()
	hunts, crashes := 0, 0
	var final []int
	for _, e := range events {
		var profile []int
		switch p := e.Data.(type) {
		case *journal.TrialIntent:
			profile = p.Profile
		case *journal.ProfileChange:
			profile, final = p.To, p.To
		case *journal.HuntStart:
			hunts++
		case *journal.CrashDetected:
			crashes++
		}
		if profile != nil {
			if name, reached := s.Reaches(profile); reached {
				t.Fatalf("%s #%d reaches %s: %v", e.Kind, e.Seq, name, profile)
			}
		}
		s.Fold(e)
	}
	if hunts > huntBudget || crashes > crashBudget {
		t.Fatalf("session used %d hunts and %d crashes, budget %d and %d", hunts, crashes, huntBudget, crashBudget)
	}
	t.Logf("session concluded after %d hunts and %d crashes", hunts, crashes)
	return final
}

func TestHuntAllZeroParkedOffsets(t *testing.T) {
	t.Parallel()
	cfg := huntConfig(16)
	cfg.Limits[11].Together[5] = -5
	stop, events, _ := runHunt(t, cfg, nil, nil)
	if stop.Reason != session.StopCycles {
		t.Fatalf("stop %+v", stop)
	}
	start, ok := findPayload(events, func(p *journal.HuntStart) bool { return p.ParkedSeq == 0 })
	if !ok {
		t.Fatal("no hunt with all-zero parked offsets")
	}
	if start.ParkedSeq != 0 {
		t.Fatalf("parked offsets seq %d", start.ParkedSeq)
	}
	end, ok := findPayload(events, func(p *journal.HuntEnd) bool { return p.Hunt == start.Hunt && p.Result == "direct" })
	if !ok || cmp.Diff([]int{11}, end.Cores) != "" {
		t.Fatalf("hunt end %+v, want direct core 11", end)
	}
	if _, ok := findPayload(events, func(p *journal.TunerDecision) bool { return p.Decision == journal.Backoff && p.Core == 11 }); !ok {
		t.Fatal("core 11 not backed off")
	}
	if _, ok := findPayload(events, func(p *journal.CheckingCycle) bool { return p.Event == journal.CycleEnd && p.Passed && p.Full }); !ok {
		t.Fatal("no passed full cycle")
	}
}

func TestHuntCulpritAfterCleanCycleParkedOffsets(t *testing.T) {
	t.Parallel()
	cfg := huntConfig(4)
	for i := range cfg.Limits {
		cfg.Limits[i].Alone = [5]int{-50, -50, -50, -50, -50}
		cfg.Limits[i].Together = [7]int{-50, -50, -50, -50, -50, -50, -50}
	}
	cfg.Joints = []sim.Joint{{Members: map[int]int{1: -20}, Regimes: []machine.Regime{machine.R6}, Rate: 10}}
	stop, events, _ := runHunt(t, cfg, nil, func(in *Input) {
		in.Config.CandidateSoloLimits = map[int]int{0: -10, 1: -10, 2: -10, 3: -10}
	})
	if stop.Reason != session.StopCycles {
		t.Fatalf("stop %+v", stop)
	}
	start, ok := findPayload(events, func(p *journal.HuntStart) bool { return p.ParkedSeq > 0 })
	if !ok {
		t.Fatal("no hunt with parked offsets from a clean cycle")
	}
	end, ok := findPayload(events, func(p *journal.HuntEnd) bool { return p.Hunt == start.Hunt && p.Result == "culprit" })
	if !ok || cmp.Diff([]int{1}, end.Cores) != "" {
		t.Fatalf("hunt end %+v, want culprit core 1", end)
	}
	if _, ok := findPayload(events, func(p *journal.TunerDecision) bool {
		return p.Core == 1 && p.Decision == journal.Backoff && p.FailurePoint != nil && p.ToOffset == *p.FailurePoint+1
	}); !ok {
		t.Fatal("no one-count-shallower backoff of core 1")
	}
}

func TestParkedOffsetBackendFailureRaisesParkedOffsets(t *testing.T) {
	t.Parallel()
	cfg := huntConfig(4)
	for i := range cfg.Limits {
		cfg.Limits[i].Alone = [5]int{-50, -50, -50, -50, -50}
		cfg.Limits[i].Together = [7]int{-50, -50, -50, -50, -50, -50, -50}
	}
	cfg.Joints = []sim.Joint{{Members: map[int]int{1: -20}, Regimes: []machine.Regime{machine.R6}, Rate: 10}}
	candidates := func(in *Input) {
		in.Config.CandidateSoloLimits = map[int]int{0: -10, 1: -10, 2: -10, 3: -10}
	}
	_, probe, _ := runHunt(t, cfg, nil, candidates)
	first, ok := findPayload(probe, func(p *journal.HuntStart) bool { return p.ParkedSeq > 0 })
	if !ok {
		t.Fatal("no parked offsets from a clean cycle")
	}
	var group *journal.HuntGroup
	var trial string
	for _, e := range probe {
		switch p := e.Data.(type) {
		case *journal.HuntGroup:
			if p.Hunt == first.Hunt && group == nil && len(p.Cores) < len(first.Candidates) {
				group = p
			}
		case *journal.TrialIntent:
			if group != nil && p.Hunt == first.Hunt && p.Group == group.Group {
				trial = p.Trial
			}
		}
		if trial != "" {
			break
		}
	}
	if group == nil || trial == "" {
		t.Fatal("no parked trial with a partial group")
	}
	held := -1
	for _, core := range first.Candidates {
		if !slices.Contains(group.Cores, core) && first.Parked[core] != 0 {
			held = core
			break
		}
	}
	if held < 0 {
		t.Fatalf("no held core at a nonzero parked offset: %+v", group)
	}
	cfg.Script = map[string]sim.Outcome{trial: {Signal: machine.ComputationError, Core: held, AtS: 1}}
	_, events, _ := runHunt(t, cfg, nil, candidates)
	end, ok := findPayload(events, func(p *journal.HuntEnd) bool {
		return p.Hunt == first.Hunt && p.Result == "direct" && cmp.Diff([]int{held}, p.Cores) == ""
	})
	if !ok {
		t.Fatalf("hunt did not directly attribute held core %d, end %+v", held, end)
	}
	if _, ok := findPayload(events, func(p *journal.TunerDecision) bool {
		return p.Core == held && p.Decision == journal.Backoff && p.FailurePoint != nil && *p.FailurePoint == first.Parked[held]
	}); !ok {
		t.Fatalf("no failure point at core %d parked offset %d", held, first.Parked[held])
	}
	next, ok := findPayload(events, func(p *journal.HuntStart) bool { return p.Hunt > first.Hunt })
	if !ok || next.ParkedSeq != first.ParkedSeq {
		t.Fatalf("next hunt %+v, want the parked offsets from #%d raised", next, first.ParkedSeq)
	}
	if next.Parked[held] <= first.Parked[held] || next.Parked[held] != next.Failing[held] {
		t.Fatalf("next parked offsets %v, want core %d raised past its failure point %d to the failing offset %d", next.Parked, held, first.Parked[held], next.Failing[held])
	}
	if slices.Contains(next.Candidates, held) {
		t.Fatalf("next candidates %v include core %d, which is no deeper than its raised parked offset", next.Candidates, held)
	}
}

func TestHuntCombination(t *testing.T) {
	t.Parallel()
	cfg := huntConfig(16)
	cfg.Ranking = []int{0, 1, 2, 11, 4, 5, 6, 7, 8, 9, 10, 3, 12, 13, 14, 15}
	model := sim.DefaultModel()
	model.PastLimitRate = 1
	cfg.Model = &model
	for i := range cfg.Limits {
		cfg.Limits[i].Alone = [5]int{-50, -50, -50, -50, -50}
		cfg.Limits[i].Together = [7]int{-50, -50, -50, -50, -50, -50, -50}
	}
	cfg.Joints = []sim.Joint{{Members: map[int]int{3: -10, 11: -10}, Regimes: []machine.Regime{machine.R6}, Rate: 10}}
	stop, events, _ := runHunt(t, cfg, nil, nil)
	if stop.Reason != session.StopCycles {
		t.Fatalf("stop %+v", stop)
	}
	combination, ok := findPayload(events, func(p *journal.Combination) bool { return !p.Fallback && len(p.Members) == 2 })
	if !ok || cmp.Diff([]journal.CombinationMember{{Core: 3, Offset: -10}, {Core: 11, Offset: -10}}, combination.Members) != "" {
		t.Fatalf("combination %+v", combination)
	}
	combinations, backoffs := 0, 0
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.Combination:
			if p.Combination == combination.Combination {
				combinations++
			}
		case *journal.TunerDecision:
			if p.FailurePoint != nil {
				t.Errorf("joint crash produced single-core failure point: %+v", p)
			}
			if p.Decision == journal.Backoff && p.Phase == journal.PhaseHunt {
				backoffs++
			}
		case *journal.TrialIntent:
			if combinations > 0 && p.Profile[3] <= -10 && p.Profile[11] <= -10 {
				t.Errorf("trial %s reaches C%d", p.Trial, combination.Combination)
			}
		}
	}
	if combinations != 1 || backoffs != 1 {
		t.Errorf("combination count %d, hunt backoffs %d, want one each", combinations, backoffs)
	}
	want := make([]int, cfg.Cores)
	for i := range want {
		want[i] = -50
	}
	want[3] = -9
	if diff := cmp.Diff(want, assertAdversarialEvidence(t, events, 1, 32)); diff != "" {
		t.Errorf("sharp joint final profile (-want +got):\n%s", diff)
	}
	t.Log("joint miss risk per 120s failing trial is exp(-1200), below 1e-500 but not zero; this fixed seed is not a universal accuracy guarantee")
}

func TestHuntJointMisleadingMCE(t *testing.T) {
	t.Parallel()
	cfg := huntConfig(16)
	model := sim.DefaultModel()
	model.PastLimitRate = 1
	cfg.Model = &model
	cfg.Joints = []sim.Joint{{Members: map[int]int{3: -10, 11: -10}, Regimes: []machine.Regime{machine.R6}, Rate: 10, CrashMCECore: new(3)}}
	stop, events, _ := runHunt(t, cfg, nil, nil)
	if stop.Reason != session.StopCycles {
		t.Fatalf("stop %+v", stop)
	}
	if _, ok := findPayload(events, func(p *journal.TunerDecision) bool {
		return p.Core == 3 && p.FailurePoint != nil && *p.FailurePoint == -10
	}); !ok {
		t.Fatal("misleading joint MCE did not attribute core 3 at -10")
	}
	if combination, ok := findPayload[*journal.Combination](events, nil); ok {
		t.Fatalf("misleading core-local evidence produced combination: %+v", combination)
	}
	want := make([]int, cfg.Cores)
	for i := range want {
		want[i] = -10
	}
	want[3] = -9
	if diff := cmp.Diff(want, assertAdversarialEvidence(t, events, 1, 32)); diff != "" {
		t.Fatalf("conservative MCE attribution final profile (-want +got):\n%s", diff)
	}
}

func TestDelayedHuntEscalates(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name  string
		after float64
		long  int
	}{
		{"idle six minutes", 360, 900},
		{"idle short-duration boundary", 120, 900},
	} {
		t.Run(tc.name, func(t *testing.T) {
			cfg := huntConfig(4)
			cfg.Joints = []sim.Joint{{Members: map[int]int{1: -10}, Regimes: []machine.Regime{machine.R6}, AfterS: tc.after, Rate: 10}}
			stop, events, _ := runHunt(t, cfg, nil, nil)
			if stop.Reason != session.StopCycles {
				t.Fatalf("delayed session stop %+v", stop)
			}
			if diff := cmp.Diff([]int{-10, -9, -10, -10}, assertAdversarialEvidence(t, events, 4, 32)); diff != "" {
				t.Fatalf("delayed sharp limit final profile (-want +got):\n%s", diff)
			}
			fullPasses, escalated, resolved := 0, false, false
			fullTrials := map[string]bool{}
			fullGroup := map[[2]int]bool{}
			for _, e := range events {
				switch p := e.Data.(type) {
				case *journal.HuntGroup:
					if p.Stage == "full" && !p.Escalated {
						fullGroup[[2]int{p.Hunt, p.Group}] = true
					}
					if p.Escalated && p.DurationS == tc.long {
						escalated = true
					}
				case *journal.TrialIntent:
					if p.Hunt > 0 && fullGroup[[2]int{p.Hunt, p.Group}] {
						fullTrials[p.Trial] = true
					}
				case *journal.TrialEnd:
					if fullTrials[p.Trial] && p.Outcome == journal.OutcomePass {
						fullPasses++
					}
				case *journal.HuntEnd:
					if (p.Result == "culprit" || p.Result == "direct") && cmp.Diff([]int{1}, p.Cores) == "" {
						resolved = true
					}
				}
			}
			if fullPasses != 5 || !escalated || !resolved {
				t.Fatalf("full passes %d, escalation %t, resolved core 1 %t", fullPasses, escalated, resolved)
			}
		})
	}
}

func TestResetReasonPowerButtonAndThermal(t *testing.T) {
	for _, tc := range []struct {
		name  string
		reset machine.ResetKind
		want  machine.Signal
		dead  journal.DeadEndCondition
	}{
		{"power button in trial", machine.ResetPowerButton, machine.Crash, ""},
		{"thermal trip", machine.ResetThermalTrip, "", journal.DeadEndThermalTrip},
	} {
		t.Run(tc.name, func(t *testing.T) {
			cfg := huntConfig(4)
			cfg.Script = map[string]sim.Outcome{"0001": {Signal: machine.Crash, Reset: tc.reset}}
			stop, events, _ := runHunt(t, cfg, nil, nil)
			crash, ok := findPayload(events, func(p *journal.CrashDetected) bool { return p.ResetReason == tc.reset })
			if !ok {
				t.Fatalf("no crash with reset reason %s", tc.reset)
			}
			if tc.dead != "" {
				if stop.Reason != session.StopDeadEnd || stop.DeadEnd.Condition != tc.dead {
					t.Fatalf("stop %+v", stop)
				}
				if !crash.Inconclusive {
					t.Fatal("thermal trip recorded as tuning failure")
				}
				return
			}
			if crash.Inconclusive {
				t.Fatal("power button during trial was inconclusive")
			}
			if _, ok := findPayload(events, func(p *journal.Failure) bool { return p.Signal == tc.want }); !ok {
				t.Fatal("power-button crash has no failure")
			}
		})
	}
}

type resetAtEvent struct {
	session.Journal
	machine *sim.Machine
	kind    journal.Kind
	reset   machine.ResetKind
	fired   *bool
}

func (j *resetAtEvent) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := j.Journal.Append(p, cause...)
	if err == nil && !*j.fired && e.Kind == j.kind {
		*j.fired = true
		j.machine.NextReset(j.reset)
		j.machine.Crash()
		return e, machine.ErrCrashed
	}
	return e, err
}

func TestThermalTripWhileTrialRunningDeadEnds(t *testing.T) {
	t.Parallel()
	fired := false
	var m *sim.Machine
	stop, events, _ := runHunt(t, huntConfig(4), func(machine *sim.Machine) { m = machine }, func(in *Input) {
		in.Wrap = func(j session.Journal) session.Journal {
			return &resetAtEvent{Journal: j, machine: m, kind: journal.KindTrialStart, reset: machine.ResetThermalTrip, fired: &fired}
		}
	})
	if !fired || stop.Reason != session.StopDeadEnd || stop.DeadEnd == nil || stop.DeadEnd.Condition != journal.DeadEndThermalTrip {
		t.Fatalf("thermal trip fired %t, stop %+v", fired, stop)
	}
	if p, ok := findPayload(events, func(p *journal.CrashDetected) bool {
		return p.ResetReason == machine.ResetThermalTrip && p.Inconclusive
	}); !ok || p.InFlight == nil {
		t.Fatalf("missing in-flight thermal crash: %+v", p)
	}
	if _, ok := findPayload(events, func(p *journal.Failure) bool { return true }); ok {
		t.Fatal("thermal trip recorded as tuning failure")
	}
}

func TestPowerButtonBetweenTrialsIsInconclusive(t *testing.T) {
	t.Parallel()
	fired := false
	var m *sim.Machine
	_, events, _ := runHunt(t, huntConfig(4), func(machine *sim.Machine) { m = machine }, func(in *Input) {
		in.Wrap = func(j session.Journal) session.Journal {
			return &resetAtEvent{Journal: j, machine: m, kind: journal.KindTrialEnd, reset: machine.ResetPowerButton, fired: &fired}
		}
		in.Until = func(e journal.Event) bool {
			p, ok := e.Data.(*journal.CrashDetected)
			return ok && p.ResetReason == machine.ResetPowerButton
		}
	})
	if !fired {
		t.Fatal("no between-trials power-button crash")
	}
	crash, ok := findPayload(events, func(p *journal.CrashDetected) bool { return p.ResetReason == machine.ResetPowerButton })
	if !ok || !crash.Inconclusive {
		t.Fatalf("power-button crash %+v, want inconclusive", crash)
	}
	if _, ok := findPayload(events, func(p *journal.Failure) bool { return true }); ok {
		t.Fatal("power-button crash between trials recorded a tuning failure")
	}
}

type jumpAtTrialStart struct {
	session.Journal
	machine *sim.Machine
	jumped  bool
}

func (j *jumpAtTrialStart) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := j.Journal.Append(p, cause...)
	if err == nil && !j.jumped && e.Kind == journal.KindTrialStart {
		j.machine.JumpWall(-2 * time.Hour)
		j.jumped = true
	}
	return e, err
}

func TestClockJumpDoesNotLoseTrialMCE(t *testing.T) {
	t.Parallel()
	cfg := huntConfig(4)
	cfg.Script = map[string]sim.Outcome{"0001": {Signal: machine.CorrectedMCE, AtS: 2, Core: 0}}
	m, err := sim.New(cfg)
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	jump := &jumpAtTrialStart{machine: m}
	in := Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Cycles: 1,
		Wrap:  func(j session.Journal) session.Journal { jump.Journal = j; return jump },
		Until: func(e journal.Event) bool { return e.Kind == journal.KindTrialEnd },
	}
	if _, err := Simulate(context.Background(), in); err != nil {
		t.Fatal(err)
	}
	events, _, err := journal.Read(dir)
	if err != nil {
		t.Fatal(err)
	}
	if !jump.jumped {
		t.Fatal("wall clock was not moved after trial start")
	}
	start, ok := findPayload(events, func(p *journal.TrialIntent) bool { return p.Trial == "0001" })
	if !ok {
		t.Fatal("missing scripted trial")
	}
	end, ok := findPayload(events, func(p *journal.TrialEnd) bool { return p.Trial == start.Trial })
	if !ok || end.Outcome != journal.OutcomeFailure || end.Signal != machine.CorrectedMCE {
		t.Fatalf("trial end %+v, want corrected MCE failure", end)
	}
	if _, ok := findPayload(events, func(p *journal.MCE) bool { return p.Corrected }); !ok {
		t.Fatal("corrected machine check was lost after the wall clock jump")
	}
}

func TestBIOSChangeArchivesAndChecksSoloLimits(t *testing.T) {
	t.Parallel()
	cfg := huntConfig(4)
	m, err := sim.New(cfg)
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	c := quickMatrixConfig()
	c.CandidateSoloLimits = map[int]int{0: -10, 1: -10, 2: -10, 3: -10}
	in := Input{Config: c, ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Cycles: 1}
	stopAfterAlonePasses(&in, 4)
	if _, err := Simulate(context.Background(), in); err != nil {
		t.Fatal(err)
	}
	old, _, err := journal.Read(dir)
	if err != nil {
		t.Fatal(err)
	}
	id := old[0].Data.(*journal.SessionStart).Session
	m.SetBIOSContext(machine.BIOSContext{BIOSVersion: "new", Board: "sim", CPUModel: "sim", Microcode: "0x2", BoostLimitMHz: 5500})
	m.Reboot()
	phases := 0
	in.Until = func(e journal.Event) bool {
		if p, ok := e.Data.(*journal.CorePhase); ok && p.From == "" {
			phases++
		}
		return phases == 4
	}
	if _, err := Simulate(context.Background(), in); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(dir, "archive", id+".jsonl")); err != nil {
		t.Fatal(err)
	}
	events, _, err := journal.Read(dir)
	if err != nil {
		t.Fatal(err)
	}
	carried, ok := findPayload(events, func(p *journal.SessionCarried) bool { return true })
	if !ok || carried.FailurePoints {
		t.Fatalf("carried %+v, want candidate solo limits only", carried)
	}
	count := 0
	for _, e := range events {
		if p, ok := e.Data.(*journal.CorePhase); ok && p.From == "" {
			if p.To != journal.PhaseSearch || !p.CheckSoloLimit {
				t.Errorf("core %d not checking carried candidate solo limit: %+v", p.Core, p)
			}
			count++
		}
	}
	if count != 4 {
		t.Errorf("initial core phases %d, want 4", count)
	}
}

func TestJournalUntilCancelsAfterFirstMatchingAppend(t *testing.T) {
	cfg := huntConfig(4)
	stop, events, _ := runHunt(t, cfg, nil, func(in *Input) {
		in.Until = func(e journal.Event) bool { return e.Kind == journal.KindTrialEnd }
	})
	if stop.Reason != session.StopSignal {
		t.Fatalf("stop %+v, want signal", stop)
	}
	ends := 0
	for _, e := range events {
		if e.Kind == journal.KindTrialEnd {
			ends++
		}
	}
	if ends != 1 {
		t.Errorf("trial ends %d, want 1", ends)
	}
}

func TestPowerLossDuringHuntResume(t *testing.T) {
	t.Parallel()
	cfg := huntConfig(4)
	cfg.Joints = []sim.Joint{{Members: map[int]int{1: -10, 3: -10}, Regimes: []machine.Regime{machine.R6}, Rate: 10}}
	runInterruptionMatrix(t, "hunt", cfg, quickMatrixConfig(),
		func(e journal.Event) bool { return e.Kind == journal.KindHuntStart },
		func(e journal.Event, closing *bool) bool {
			if e.Kind == journal.KindHuntEnd {
				*closing = true
			}
			p, ok := e.Data.(*journal.TrialIntent)
			return *closing && ok && !p.Rerun
		})
}

func TestPowerLossDuringDeepeningResume(t *testing.T) {
	t.Parallel()
	cfg := huntConfig(4)
	for i := range cfg.Limits {
		cfg.Limits[i].Alone = [5]int{-50, -50, -50, -50, -50}
		cfg.Limits[i].Together = [7]int{-50, -50, -50, -50, -50, -50, -50}
	}
	c := quickMatrixConfig()
	runInterruptionMatrix(t, "deepening", cfg, c,
		func(e journal.Event) bool {
			p, ok := e.Data.(*journal.DeepeningRound)
			return ok && p.Event == journal.CycleStart
		},
		func(e journal.Event, _ *bool) bool {
			p, ok := e.Data.(*journal.DeepeningRound)
			return ok && p.Event == journal.CycleEnd
		})
}

func quickMatrixConfig() config.Config {
	c := config.Default()
	c.CandidateSoloLimits = map[int]int{0: -10, 1: -10, 2: -10, 3: -10}
	c.Evidence.Miss = .5
	c.Durations.SearchTrialS = 1
	c.Durations.ShortTrialS = 1
	c.Durations.CheckingTrialS = 1
	c.Durations.CheckingIdleS = 1
	c.Durations.CheckingAllCoreS = 1
	return c
}

func runInterruptionMatrix(t *testing.T, name string, cfg sim.Config, c config.Config, open func(journal.Event) bool, closeWindow func(journal.Event, *bool) bool) {
	t.Helper()
	_, prefix, _ := runHunt(t, cfg, nil, func(in *Input) {
		in.Config = c
		in.Cycles = 0
		in.Until = open
	})
	openIndex := slices.IndexFunc(prefix, open)
	if openIndex < 0 {
		t.Fatalf("%s prefix missing opening event", name)
	}
	matrix := interruptionMatrix{name: name, cfg: cfg, config: c, prefix: prefix[:openIndex+1], closeWindow: closeWindow}
	reference, accesses := matrix.run(t, -1, -1)
	window := reference[len(matrix.prefix):]
	want := matrixCommitments(reference)
	if name == "hunt" && len(want.Reruns) == 0 {
		t.Fatal("hunt reference has no completed rerun evidence")
	}
	if name == "hunt" {
		for _, rerun := range want.Reruns {
			if rerun.Outcome != journal.OutcomePass {
				t.Fatalf("hunt reference rerun did not pass: %+v", rerun)
			}
		}
	}
	closing := false
	closeAt := slices.IndexFunc(window, func(e journal.Event) bool { return closeWindow(e, &closing) }) + 1
	if closeAt == 0 {
		t.Fatalf("%s reference missing closing event", name)
	}
	for at := 0; at <= closeAt; at++ {
		t.Run(fmt.Sprintf("append %d", at), func(t *testing.T) {
			t.Parallel()
			events, _ := matrix.run(t, at, -1)
			assertMatrixDecisions(t, want, events)
		})
	}
	for i, access := range accesses {
		t.Run(fmt.Sprintf("smu %d %s core %d after %v", i+1, access.Op, access.Core, access.After), func(t *testing.T) {
			t.Parallel()
			events, observed := matrix.run(t, -1, i+1)
			if diff := cmp.Diff(access, observed[i]); diff != "" {
				t.Fatalf("interrupted another SMU window (-want +got):\n%s", diff)
			}
			assertMatrixDecisions(t, want, events)
		})
	}
}

type matrixCrash struct {
	session.Journal
	machine *sim.Machine
	at      int
	count   *int
	crashed *bool
}

func (j *matrixCrash) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := j.Journal.Append(p, cause...)
	if err == nil {
		*j.count++
		if *j.count == j.at {
			*j.crashed = true
			j.machine.NextReset(machine.ResetPowerLoss)
			j.machine.Crash()
			return e, machine.ErrCrashed
		}
	}
	return e, err
}

func TestScriptedJointAndIdleLimits(t *testing.T) {
	for _, tc := range []struct {
		name string
		idle bool
		want []int
	}{
		{"sharp joint", false, []int{-5, -10, -10, -10}},
		{"idle-only outside loaded cores", true, []int{-10, -10, -10, -5}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			cfg := huntConfig(4)
			cfg.Ranking = []int{2, 1, 0, 3}
			cfg.Script = map[string]sim.Outcome{}
			if tc.idle {
				cfg.Limits[3].Idle = new(-5)
			} else {
				cfg.Joints = []sim.Joint{{Members: map[int]int{0: -6, 2: -6}, Regimes: []machine.Regime{machine.R6}, Rate: 10}}
			}
			stop, events, _ := runHunt(t, cfg, nil, func(in *Input) {
				if tc.idle {
					checkingR1First(in)
				}
				in.Until = func(e journal.Event) bool {
					p, ok := e.Data.(*journal.TrialIntent)
					if !ok {
						return false
					}
					cfg.Script[p.Trial] = scriptedSharpOutcome(cfg, p)
					return false
				}
			})
			if stop.Reason != session.StopCycles {
				t.Fatalf("scripted session stop %+v", stop)
			}
			huntBudget, crashBudget := 4, 32
			if tc.idle {
				huntBudget = *cfg.Limits[3].Idle - cfg.Limits[3].Together[0]
				crashBudget = 2*cfg.Cores + 3*huntBudget
				t.Logf("idle bound: %d one-count repairs, at most one source and two failing bisections each, plus two search failures alone per core", huntBudget)
			}
			if diff := cmp.Diff(tc.want, assertAdversarialEvidence(t, events, huntBudget, crashBudget)); diff != "" {
				t.Fatalf("deterministic hidden limits (-want +got):\n%s", diff)
			}
			if tc.idle {
				intents := map[string]*journal.TrialIntent{}
				found := false
				for _, e := range events {
					switch p := e.Data.(type) {
					case *journal.TrialIntent:
						intents[p.Trial] = p
					case *journal.Failure:
						tr := intents[p.Trial]
						if tr != nil && tr.Regime == machine.R1 && tr.Condition == machine.Together && tr.Profile[3] < *cfg.Limits[3].Idle && !slices.Contains(tr.Cores, 3) && (tr.Core == nil || *tr.Core != 3) && p.Core != nil && *p.Core == 3 && p.Signal == machine.ComputationError {
							found = true
						}
					}
				}
				if !found {
					t.Fatal("no named R1 idle-limit failure outside the loaded cores")
				}
			}
		})
	}
}

func scriptedSharpOutcome(cfg sim.Config, p *journal.TrialIntent) sim.Outcome {
	loaded := p.Cores
	if p.Core != nil {
		loaded = []int{*p.Core}
	}
	for _, core := range loaded {
		regime := slices.Index(machine.Regimes, p.Regime)
		limit := cfg.Limits[core].Together[regime]
		if p.Condition == machine.Alone {
			limit = cfg.Limits[core].Alone[regime]
		}
		if p.Profile[core] < limit {
			return sim.Outcome{Signal: machine.ComputationError, AtS: 1, Core: core}
		}
	}
	if p.Condition == machine.Alone {
		return sim.Outcome{}
	}
	for core, limit := range cfg.Limits {
		if limit.Idle != nil && !slices.Contains(loaded, core) && p.Profile[core] < *limit.Idle {
			return sim.Outcome{Signal: machine.ComputationError, AtS: 1, Core: core}
		}
	}
	for _, combination := range cfg.Joints {
		if !slices.Contains(combination.Regimes, p.Regime) {
			continue
		}
		reached := true
		for core, offset := range combination.Members {
			reached = reached && p.Profile[core] <= offset
		}
		if reached {
			return sim.Outcome{Signal: machine.Crash, AtS: 1}
		}
	}
	return sim.Outcome{}
}

func checkingR1First(in *Input) {
	// Keep every occurrence while exercising idle cores before multi-core R7.
	cycle := in.Config.Checking.Cycle
	first := slices.Index(cycle, machine.R1)
	cycle[0], cycle[first] = cycle[first], cycle[0]
}

func TestIdleOnlyHazardReachesCleanCycle(t *testing.T) {
	for _, seed := range []uint64{1, 2} {
		t.Run(fmt.Sprintf("seed-%d", seed), func(t *testing.T) {
			cfg := huntConfig(4)
			cfg.Seed = seed
			cfg.Limits[3].Idle = new(-5)
			stop, events, _ := runHunt(t, cfg, nil, checkingR1First)
			if stop.Reason != session.StopCycles {
				t.Fatalf("idle-only stop %+v", stop)
			}
			huntBudget := *cfg.Limits[3].Idle - cfg.Limits[3].Together[0]
			crashBudget := 2*cfg.Cores + 3*huntBudget
			t.Logf("idle bound: %d one-count repairs, at most one source and two failing bisections each, plus two search failures alone per core", huntBudget)
			if diff := cmp.Diff([]int{-10, -10, -10, -5}, assertAdversarialEvidence(t, events, huntBudget, crashBudget)); diff != "" {
				t.Fatalf("sharp idle limit at fixed seed (-want +got):\n%s", diff)
			}
			intents := map[string]*journal.TrialIntent{}
			found := false
			for _, e := range events {
				switch p := e.Data.(type) {
				case *journal.TrialIntent:
					intents[p.Trial] = p
				case *journal.Failure:
					tr := intents[p.Trial]
					if tr != nil && tr.Regime == machine.R1 && tr.Condition == machine.Together && tr.Profile[3] < *cfg.Limits[3].Idle && !slices.Contains(tr.Cores, 3) && (tr.Core == nil || *tr.Core != 3) {
						found = true
					}
				}
			}
			if !found {
				t.Fatal("idle-only hazard never failed outside the loaded cores")
			}
			t.Logf("zero near-limit rate, eligible idle failing hazard at least 1/s: a 120s failing trial misses with probability at most exp(-120) = %g; equality is a fixed-seed regression, not a universal guarantee", math.Exp(-120))
		})
	}
}
