package tuner_test

import (
	"context"
	"fmt"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/simrun"
	"github.com/shgew/togi/internal/tuner"
)

func simulatedForecastJournal(tb testing.TB, seed uint64, cores int) []journal.Event {
	tb.Helper()
	model := sim.DefaultModel()
	model.PastLimitRate = 1
	model.Signals = map[machine.Signal]float64{machine.ComputationError: 1}
	limits := make([]sim.Limits, cores)
	for i := range limits {
		limits[i].Alone = [5]int{-10, -10, -10, -10, -10}
		limits[i].Together = [7]int{-10, -10, -10, -10, -10, -10, -10}
	}
	cfg := sim.Config{Seed: seed, Cores: cores, Limits: limits, Model: &model}
	if seed == 7 {
		cfg.Joints = []sim.Joint{{Members: map[int]int{0: -10, 1: -10}, Regimes: []machine.Regime{machine.R7}, Rate: 1, Signal: machine.Crash}}
		model.CrashMCE = 0
	}
	if cores == 16 {
		cfg = sim.Config{Seed: seed}
	}
	m, err := sim.New(cfg)
	if err != nil {
		tb.Fatal(err)
	}
	dir := tb.TempDir()
	_, err = simrun.Simulate(context.Background(), simrun.Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Cycles: 1, InMemoryJournal: true})
	if err != nil {
		tb.Fatal(err)
	}
	events, torn, err := journal.Read(dir)
	if err != nil || torn != nil {
		tb.Fatalf("read simulation: %v, torn %q", err, torn)
	}
	return events
}

func forecastDecision(p journal.Payload) bool {
	switch p.Kind() {
	case journal.KindFailure, journal.KindTunerDecision, journal.KindCorePhase, journal.KindCheckingCycle, journal.KindCheckingStep, journal.KindProfileChange, journal.KindHuntStart, journal.KindHuntGroup, journal.KindHuntEnd, journal.KindHuntSkipped, journal.KindCombination, journal.KindDeepeningRound, journal.KindTunerWarning, journal.KindDeadEnd:
		return true
	}
	return false
}

func TestForecastMatchesSimulatedTrials(t *testing.T) {
	options := cmp.Options{
		cmpopts.IgnoreFields(journal.Failure{}, "Signal", "Trial"),
		cmpopts.IgnoreFields(journal.TunerDecision{}, "Reason"),
		cmpopts.IgnoreFields(journal.CorePhase{}, "Reason"),
		cmpopts.IgnoreFields(journal.CheckingCycle{}, "Reason"),
		cmpopts.IgnoreFields(journal.HuntStart{}, "Failure", "ParkedSeq", "Trial"),
		cmpopts.IgnoreFields(journal.HuntGroup{}, "Reason"),
		cmpopts.IgnoreFields(journal.HuntEnd{}, "Reason"),
		cmpopts.IgnoreFields(journal.DeepeningRound{}, "BaseSeq", "Reason"),
		cmpopts.IgnoreFields(journal.TunerWarning{}, "Trial", "Passes", "Detail"),
	}
	for _, seed := range []uint64{1, 7} {
		t.Run(fmt.Sprint(seed), func(t *testing.T) {
			events := simulatedForecastJournal(t, seed, 2)
			checked, missing := 0, 0
			for i, e := range events {
				intent, ok := e.Data.(*journal.TrialIntent)
				if !ok {
					continue
				}
				endAt := -1
				var end *journal.TrialEnd
				for j := i + 1; j < len(events); j++ {
					if p, ok := events[j].Data.(*journal.TrialEnd); ok && p.Trial == intent.Trial {
						endAt = j
						end = p
						break
					}
				}
				if end == nil {
					t.Fatalf("trial %s has no end", intent.Trial)
				}
				premise := tuner.IfPass
				actualCore := end.Core
				if end.Outcome == journal.OutcomeInconclusive {
					premise = tuner.IfInconclusive
				} else if end.Outcome == journal.OutcomeFailure {
					premise = tuner.IfUnnamed
					if end.Core != nil || intent.Core != nil && intent.Condition == machine.Alone {
						premise = tuner.IfNamed
					}
					for j := endAt + 1; j < len(events); j++ {
						if events[j].Kind == journal.KindTrialIntent {
							break
						}
						if failure, ok := events[j].Data.(*journal.Failure); ok && failure.Trial == intent.Trial {
							actualCore = failure.Core
							if failure.Attribution == journal.Attributed {
								premise = tuner.IfNamed
							}
							break
						}
					}
				}
				f := tuner.Forecast(events[:i+1])
				var branch *tuner.ForecastBranch
				for k := range f.Branches {
					if f.Branches[k].Premise == premise {
						branch = &f.Branches[k]
						break
					}
				}
				if branch == nil {
					t.Fatalf("trial %s lacks premise %s: intent=%+v end=%+v", intent.Trial, premise, intent, end)
				}
				if premise == tuner.IfNamed && actualCore != nil && branch.Core != nil && *actualCore != *branch.Core {
					missing++
					continue
				}
				var decisions []journal.Payload
				var next *journal.TrialIntent
				var lastRanking []int
				for j := 0; j <= i; j++ {
					if p, ok := events[j].Data.(*journal.HostRanking); ok {
						lastRanking = p.Ranking
					}
				}
				rankingChanged := false
				for j := endAt + 1; j < len(events); j++ {
					p := events[j].Data
					if v, ok := p.(*journal.TrialIntent); ok {
						next = v
						break
					}
					if p, ok := p.(*journal.HostRanking); ok {
						rankingChanged = cmp.Diff(lastRanking, p.Ranking) != ""
					}
					if forecastDecision(p) {
						decisions = append(decisions, p)
					}
					if p.Kind() == journal.KindShutdown {
						break
					}
				}
				if branch.NeedsRanking {
					missing++
					continue
				}
				if rankingChanged {
					missing++
					continue
				}
				if next == nil && end.Outcome == journal.OutcomePass {
					// The simulator's requested-cycle stop is runtime policy, not a tuner decision.
					if len(branch.Decisions) < len(decisions) {
						t.Fatalf("trial %s lost decisions before stop", intent.Trial)
					}
					if diff := cmp.Diff(decisions, branch.Decisions[:len(decisions)], options...); diff != "" {
						t.Fatal(diff)
					}
					continue
				}
				if diff := cmp.Diff(decisions, branch.Decisions, options...); diff != "" {
					t.Fatalf("trial %s decisions (-journal +forecast):\n%s", intent.Trial, diff)
				}
				if next == nil {
					if branch.Next != nil { // Requested-cycle stops are runtime policy, not tuner decisions.
						if end.Outcome == journal.OutcomePass {
							continue
						}
						t.Fatalf("trial %s forecast ran after stop", intent.Trial)
					}
					continue
				}
				if branch.Next == nil {
					t.Fatalf("trial %s forecast stopped before %s", intent.Trial, next.Trial)
				}
				tr := *branch.Next
				predicted, _, err := tr.Complete(0, next.Profile)
				if err != nil {
					t.Fatal(err)
				}
				predicted.Trial = next.Trial
				if diff := cmp.Diff(next, predicted, cmpopts.IgnoreFields(journal.TrialIntent{}, "KernelBoundary")); diff != "" {
					t.Fatalf("trial %s next (-journal +forecast):\n%s", intent.Trial, diff)
				}
				checked++
			}
			if checked < 50 {
				t.Fatalf("only %d predictions checked (%d unavailable inputs)", checked, missing)
			}
			t.Logf("matched %d trial transitions; %d required unavailable inputs", checked, missing)
		})
	}
}

func BenchmarkDashboardProjectionForecast(b *testing.B) {
	events := simulatedForecastJournal(b, 1, 16)
	// Cut at the last intent to benchmark every conditional branch, not a stopped session.
	for i := len(events) - 1; i >= 0; i-- {
		if events[i].Kind == journal.KindTrialIntent {
			events = events[:i+1]
			break
		}
	}
	b.ResetTimer()
	for b.Loop() {
		s := tuner.New()
		for _, e := range events {
			s.Fold(e)
		}
		for id := range 16 {
			_ = s.CoreLimit(id)
		}
		_ = s.CyclePlan()
		_ = s.HuntPlan()
		_ = s.SearchTurns()
		_ = s.DeepeningPlan()
		_ = tuner.Forecast(events)
	}
	b.ReportMetric(float64(len(events)), "events")
}
