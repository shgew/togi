package tuner_test

import (
	"context"
	"fmt"
	"slices"
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
	switch p.(type) {
	case *journal.Failure, *journal.TunerDecision, *journal.CorePhase, *journal.CheckingCycle, *journal.CheckingStep, *journal.CheckingChain, *journal.ProfileChange, *journal.HuntStart, *journal.HuntGroup, *journal.HuntEnd, *journal.HuntSkipped, *journal.Combination, *journal.DeepeningRound, *journal.TunerWarning, *journal.DeadEnd:
		return true
	}
	return false
}

// journalAllPass follows the journal past the trial ending at endAt while the next trials pass, and returns the
// decisions recorded until the trial after the last of passes, and that trial. ok is false when one of them did not
// pass, the host ranking changed, or the session stopped first.
func journalAllPass(events []journal.Event, endAt, passes int, lastRanking []int) (decisions []journal.Payload, next *journal.TrialIntent, ok bool) {
	ran := 1
	for j := endAt + 1; j < len(events); j++ {
		switch p := events[j].Data.(type) {
		case *journal.TrialIntent:
			if ran == passes {
				return decisions, p, true
			}
			end := -1
			for k := j + 1; k < len(events); k++ {
				if e, isEnd := events[k].Data.(*journal.TrialEnd); isEnd && e.Trial == p.Trial {
					if e.Outcome != journal.OutcomePass {
						return nil, nil, false
					}
					end = k
					break
				}
			}
			if end < 0 {
				return nil, nil, false
			}
			ran++
			j = end
			continue
		case *journal.HostRanking:
			if cmp.Diff(lastRanking, p.Ranking) != "" {
				return nil, nil, false
			}
		case *journal.Shutdown:
			return nil, nil, false
		}
		if forecastDecision(events[j].Data) {
			decisions = append(decisions, events[j].Data)
		}
	}
	return nil, nil, false
}

func TestForecastMatchesSimulatedTrials(t *testing.T) {
	options := cmp.Options{
		cmpopts.IgnoreFields(journal.Failure{}, "Signal", "Trial"),
		cmpopts.IgnoreFields(journal.TunerDecision{}, "Reason"),
		cmpopts.IgnoreFields(journal.CorePhase{}, "Reason"),
		cmpopts.IgnoreFields(journal.CheckingCycle{}, "Reason"),
		cmpopts.IgnoreFields(journal.CheckingChain{}, "Msg"),
		cmpopts.IgnoreFields(journal.HuntStart{}, "Failure", "ParkedSeq", "Trial"),
		cmpopts.IgnoreFields(journal.HuntGroup{}, "Reason"),
		cmpopts.IgnoreFields(journal.HuntEnd{}, "Reason"),
		cmpopts.IgnoreFields(journal.DeepeningRound{}, "BaseSeq", "Reason"),
		cmpopts.IgnoreFields(journal.TunerWarning{}, "Trial", "Passes", "Detail"),
	}
	matchNext := func(t *testing.T, trial string, next *journal.TrialIntent, forecast *tuner.Trial) {
		t.Helper()
		if forecast == nil {
			t.Fatalf("trial %s forecast stopped before %s", trial, next.Trial)
		}
		predicted, _, err := forecast.Complete(0, next.Profile)
		if err != nil {
			t.Fatal(err)
		}
		predicted.Trial = next.Trial
		if diff := cmp.Diff(next, predicted, cmpopts.IgnoreFields(journal.TrialIntent{}, "KernelBoundary")); diff != "" {
			t.Fatalf("trial %s next (-journal +forecast):\n%s", trial, diff)
		}
	}
	for _, run := range []struct {
		seed         uint64
		cores, every int
	}{{1, 2, 1}, {7, 2, 1}, {1, 16, 8}} {
		t.Run(fmt.Sprintf("%d/%d cores", run.seed, run.cores), func(t *testing.T) {
			events := simulatedForecastJournal(t, run.seed, run.cores)
			checked, allChecked, missing, intents := 0, 0, 0, 0
			s := tuner.New()
			folded := 0
			var ranking []int
			for i, e := range events {
				if p, ok := e.Data.(*journal.HostRanking); ok {
					ranking = p.Ranking
				}
				intent, ok := e.Data.(*journal.TrialIntent)
				if !ok {
					continue
				}
				intents++
				if intents%run.every != 0 {
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
				if intent.Regime == machine.R7 && len(intent.Cores) > 1 && end.Outcome != journal.OutcomeInconclusive &&
					(len(end.VoltageRequestsV) > 0 || len(end.TopRequesters) > 0 || len(end.CCDMHz) > 0) {
					// Forecast ends the current trial without fabricated telemetry. Its branches use earlier
					// measurements; a real end can reorder requests, change voltage targets or derive a
					// different partial. That future telemetry is not an input available at this intent.
					// The unit fixture separately pins these branches against telemetry-free tuner folds.
					missing++
					continue
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
				var branch, all *tuner.ForecastBranch
				named := false
				for k := range f.Branches {
					b := &f.Branches[k]
					if b.NeedsHistory {
						t.Fatalf("complete simulated history recurred at trial %s, premise %s", intent.Trial, b.Premise)
					}
					if b.Premise == tuner.IfAllPass {
						all = b
					}
					if b.Premise != premise {
						continue
					}
					if premise == tuner.IfNamed {
						named = true
						// Each named branch follows one core; compare the one the journal names.
						if actualCore == nil || b.Core == nil || *actualCore != *b.Core {
							continue
						}
					}
					branch = b
				}
				if branch == nil && named {
					missing++
					continue
				}
				if branch == nil {
					t.Fatalf("trial %s lacks premise %s: intent=%+v end=%+v", intent.Trial, premise, intent, end)
				}
				var decisions []journal.Payload
				var next *journal.TrialIntent
				nextAt := -1
				lastRanking := ranking
				if all != nil && end.Outcome == journal.OutcomePass && !all.NeedsRanking {
					if decisions, next, ok := journalAllPass(events, endAt, all.Passes, lastRanking); ok {
						if diff := cmp.Diff(decisions, all.Decisions, options...); diff != "" {
							t.Fatalf("trial %s all-pass decisions (-journal +forecast):\n%s", intent.Trial, diff)
						}
						matchNext(t, intent.Trial, next, all.Next)
						allChecked++
					}
				}
				rankingChanged := false
				for j := endAt + 1; j < len(events); j++ {
					p := events[j].Data
					if v, ok := p.(*journal.TrialIntent); ok {
						next, nextAt = v, j
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
				matchNext(t, intent.Trial, next, branch.Next)
				if next.Phase == journal.PhaseChecking && next.Cycle > 0 && !next.Rerun && next.Hunt == 0 {
					if nextAt+1 < folded {
						t.Fatalf("trial %s next intent at %d precedes folded prefix %d", intent.Trial, nextAt, folded)
					}
					for ; folded <= nextAt; folded++ {
						s.Fold(events[folded])
					}
					if want := s.CyclePlan().Current + 1; branch.NextStep != want {
						t.Fatalf("trial %s next step %d, want %d when it runs", intent.Trial, branch.NextStep, want)
					}
				}
				checked++
			}
			if checked < 50 || allChecked == 0 {
				t.Fatalf("only %d predictions and %d all-pass predictions checked (%d unavailable inputs)", checked, allChecked, missing)
			}
			t.Logf("matched %d trial transitions and %d all-pass runs; %d required unavailable inputs", checked, allChecked, missing)
		})
	}
}

func BenchmarkDashboardProjectionForecast(b *testing.B) {
	events := simulatedForecastJournal(b, 1, 16)
	// Cut at the last intent to benchmark every conditional branch, not a stopped session.
	for i, e := range slices.Backward(events) {
		if e.Kind == journal.KindTrialIntent {
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
