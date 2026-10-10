package simrun

import (
	"fmt"
	"math"
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/tuner"
)

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
	joint := []sim.Joint{{Members: map[int]int{1: -10}, Regimes: []machine.Regime{machine.R6}, Rate: 10}}
	stop, events, _ := runHuntAfterConfirmation(t, cfg, joint, func(in *Input) {
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
	joint := []sim.Joint{{Members: map[int]int{1: -10}, Regimes: []machine.Regime{machine.R6}, Rate: 10}}
	candidates := func(in *Input) {
		in.Config.CandidateSoloLimits = map[int]int{0: -10, 1: -10, 2: -10, 3: -10}
	}
	_, probe, _ := runHuntAfterConfirmation(t, cfg, joint, candidates)
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
	_, events, _ := runHuntAfterConfirmation(t, cfg, joint, candidates)
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
			joint := []sim.Joint{{Members: map[int]int{1: -10}, Regimes: []machine.Regime{machine.R6}, AfterS: tc.after, Rate: 10}}
			stop, events, _ := runHuntAfterConfirmation(t, cfg, joint, func(*Input) {})
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
			if tc.idle {
				cfg.Limits[3].Idle = new(-5)
			} else {
				cfg.Joints = []sim.Joint{{Members: map[int]int{0: -6, 2: -6}, Regimes: []machine.Regime{machine.R6}, Rate: 10}}
			}
			var m *sim.Machine
			stop, events, _ := runHunt(t, cfg, func(machine *sim.Machine) { m = machine }, func(in *Input) {
				if tc.idle {
					checkingR1First(in)
				}
				in.Until = func(e journal.Event) bool {
					p, ok := e.Data.(*journal.TrialIntent)
					if !ok {
						return false
					}
					if err := m.ScriptTrial(p.Trial, conditionLimitOutcome(cfg, p)); err != nil {
						t.Error(err)
					}
					return false
				}
			})
			if stop.Reason != session.StopCycles {
				t.Fatalf("scripted session stop %+v", stop)
			}
			huntBudget, crashBudget := 4, 32
			if tc.idle {
				huntBudget, crashBudget = idleBudgets(t, cfg, 3)
			}
			if diff := cmp.Diff(tc.want, assertAdversarialEvidence(t, events, huntBudget, crashBudget)); diff != "" {
				t.Fatalf("deterministic hidden limits (-want +got):\n%s", diff)
			}
			if tc.idle && !slices.ContainsFunc(idleFailures(events, 3, *cfg.Limits[3].Idle), func(p *journal.Failure) bool {
				return p.Core != nil && *p.Core == 3 && p.Signal == machine.ComputationError
			}) {
				t.Fatal("no named R1 idle-limit failure outside the loaded cores")
			}
		})
	}
}

// conditionLimitOutcome is a deterministic oracle over cfg's hidden limits: the first loaded core past its limit fails
// at 1 s; else, outside trials alone, the first core past its idle limit; else any reached joint crashes. Unlike the
// simulator, it picks the alone or together limit by the trial's condition instead of by its profile, and ignores
// workload limits, flat rates, joint delays and joint signals.
func conditionLimitOutcome(cfg sim.Config, p *journal.TrialIntent) sim.Outcome {
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

func idleBudgets(t *testing.T, cfg sim.Config, core int) (hunts, crashes int) {
	t.Helper()
	hunts = *cfg.Limits[core].Idle - cfg.Limits[core].Together[0]
	crashes = 2*cfg.Cores + 3*hunts
	t.Logf("idle bound: %d one-count repairs, at most one source and two failing bisections each, plus two search failures alone per core", hunts)
	return hunts, crashes
}

// idleFailures returns the failures of R1 trials together that did not load core but ran it past its idle limit.
func idleFailures(events []journal.Event, core, idle int) []*journal.Failure {
	intents := map[string]*journal.TrialIntent{}
	var failures []*journal.Failure
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.TrialIntent:
			intents[p.Trial] = p
		case *journal.Failure:
			tr := intents[p.Trial]
			if tr != nil && tr.Regime == machine.R1 && tr.Condition == machine.Together && tr.Profile[core] < idle && !slices.Contains(tr.Cores, core) && (tr.Core == nil || *tr.Core != core) {
				failures = append(failures, p)
			}
		}
	}
	return failures
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
			huntBudget, crashBudget := idleBudgets(t, cfg, 3)
			if diff := cmp.Diff([]int{-10, -10, -10, -5}, assertAdversarialEvidence(t, events, huntBudget, crashBudget)); diff != "" {
				t.Fatalf("sharp idle limit at fixed seed (-want +got):\n%s", diff)
			}
			if len(idleFailures(events, 3, *cfg.Limits[3].Idle)) == 0 {
				t.Fatal("idle-only hazard never failed outside the loaded cores")
			}
			t.Logf("zero near-limit rate, eligible idle failing hazard at least 1/s: a 120s failing trial misses with probability at most exp(-120) = %g; equality is a fixed-seed regression, not a universal guarantee", math.Exp(-120))
		})
	}
}
