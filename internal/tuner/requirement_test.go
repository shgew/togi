package tuner

import (
	"fmt"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestStoredRequirementSurvivesCrashAndChangedConfig(t *testing.T) {
	cfg := config.Default()
	h := newHarnessOn(t, topology(1), cfg, coreStart{phase: journal.PhaseSearch, offset: -20, check: true})
	for range cfg.Evidence.Trials() - 1 {
		h.trial(h.next(), passed)
	}
	a := h.next()
	p := h.start(a).Data.(*journal.TrialIntent)
	want := TrialRequirement{Passed: cfg.Evidence.Trials() - 1, Trial: cfg.Evidence.Trials(), Needed: cfg.Evidence.Trials()}
	if diff := cmp.Diff(a.Trial.Requirement, h.s.StoredRequirement(p.Trial)); diff != "" {
		t.Fatal(diff)
	}
	cfg.Evidence.Miss = 0.001
	cfg.Durations.SearchTrialS++
	h.add(&journal.ConfigLoaded{Config: snapshotConfig(cfg)})
	if diff := cmp.Diff(want, h.s.Requirement(p)); diff != "" {
		t.Fatalf("config reload reinterpreted the running requirement:\n%s", diff)
	}
	for _, b := range Forecast(h.events).Branches {
		if b.Premise == IfAllPass {
			t.Fatal("forecast used the resumed config's larger needed count")
		}
	}
	h.add(&journal.CrashDetected{})
	h.add(&journal.TrialEnd{Trial: p.Trial, Outcome: journal.OutcomeFailure, Signal: machine.Crash})
	if diff := cmp.Diff(want, h.s.TrialHistory(p.Trial).Requirement); diff != "" {
		t.Fatalf("crash closure reinterpreted the historical requirement:\n%s", diff)
	}
	replayed := replayState(h.events)
	if diff := cmp.Diff(h.s.TrialHistory(p.Trial), replayed.TrialHistory(p.Trial)); diff != "" {
		t.Fatalf("history changed across replay:\n%s", diff)
	}
	h.add(&journal.CommandReset{Core: new(0)})
	if diff := cmp.Diff(want, h.s.TrialHistory(p.Trial).Requirement); diff != "" {
		t.Fatalf("reset erased historical counts:\n%s", diff)
	}
	if diff := cmp.Diff(want, h.s.Requirement(p)); diff != "" {
		t.Fatalf("historical requirement counted evidence after its trial closed:\n%s", diff)
	}
	if diff := cmp.Diff(a.Trial.Requirement, h.s.StoredRequirement(p.Trial)); diff != "" {
		t.Fatalf("stored obligation changed:\n%s", diff)
	}
}

func TestStoredRequirementSeparatesClassCountAndPart(t *testing.T) {
	h := chainHarness(t)
	passChainPart(t, h, []int{0, 1, 2, 3}, nil)
	h.decide(h.s.cycleNext())
	a := h.s.cycleNext()
	p := h.start(a).Data.(*journal.TrialIntent)
	stored := h.s.StoredRequirement(p.Trial)
	if stored.Step != 1 || stored.Part != 2 || stored.Needed != 3 {
		t.Fatalf("partial identity and class count: %+v", stored)
	}
	h.add(&journal.TrialEnd{Trial: p.Trial, Outcome: journal.OutcomeInconclusive})
	history := h.s.TrialHistory(p.Trial)
	if history.Requirement.Needed != 3 || history.PartNeeded != 4 || history.PartTrial != 1 {
		t.Fatalf("class count was replaced by the combined part count: %+v", history)
	}
}

func TestRetryRequirementMatchesFold(t *testing.T) {
	for _, tc := range []struct {
		name      string
		step      machine.Regime
		change    func(*config.Config)
		wantRetry bool
	}{
		{"same shape cycle", machine.R1, func(*config.Config) {}, true},
		// A resume that changes the duration changes the class the step requires: the retry is dropped and the current
		// requirement runs.
		{"stale cycle duration", machine.R1, func(c *config.Config) { c.Durations.CheckingTrialS++ }, false},
		{"same shape R7 part", machine.R7, func(*config.Config) {}, true},
		{"stale R7 part duration", machine.R7, func(c *config.Config) { c.Durations.ShortTrialS++ }, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			cfg := config.Default()
			var h *harness
			if tc.step == machine.R7 {
				h = chainHarnessOn(t, topology(8), cfg, tc.step)
			} else {
				h = newHarnessOn(t, topology(8), cfg, hasRoomStarts(-10, -20, -30, -40, -20, -20, -20, -20)...)
				h.decide(h.next())
				h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{tc.step}})
			}
			first := h.s.cycleNext()
			if first.Kind != RunTrial || first.Trial.Requirement.Kind != "cycle" || first.Trial.Requirement.Step != 1 || first.Trial.Requirement.Part != 1 {
				t.Fatalf("first trial's selected requirement: %+v", first)
			}
			h.trial(first, unsure)
			tc.change(&cfg)
			h.add(&journal.ConfigLoaded{Config: snapshotConfig(cfg)})
			retry := h.s.cycleNext()
			if retry.Kind != RunTrial || retry.Trial.Retry != tc.wantRetry {
				t.Fatalf("retry = %t, want %t: %+v", retry.Trial.Retry, tc.wantRetry, retry)
			}
			p := h.start(retry).Data.(*journal.TrialIntent)
			if diff := cmp.Diff(retry.Trial.Requirement, h.s.StoredRequirement(p.Trial)); diff != "" {
				t.Fatalf("attached requirement differs from the fold's (-attached +stored):\n%s", diff)
			}
		})
	}
}

// TestZeroRerunRequirementIsOneTrial stores an all-zero rerun's requirement as its one conclusive trial: earlier live
// passes of its class at deeper profiles, and an unrelated rerun obligation, neither lengthen it nor advance its number.
func TestZeroRerunRequirementIsOneTrial(t *testing.T) {
	h := hasRoomHarness(t, 0, -10)
	w := machine.Workloads(machine.R5)[0].ID
	tr := Trial{Core: 0, Regime: machine.R5, Workload: w, Phase: journal.PhaseChecking, Condition: machine.Together, DurationS: 120}
	h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
	h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(0)})
	failure := h.decide(h.next())
	a := h.next()
	p := h.start(a).Data.(*journal.TrialIntent)
	if p.Condition != machine.Parked || !p.Rerun {
		t.Fatalf("next trial is not the all-zero rerun: %+v", p)
	}
	stored := h.s.StoredRequirement(p.Trial)
	if stored.Kind != "zero-rerun" || stored.Since != failure.Seq || stored.Needed != 1 {
		t.Fatalf("stored requirement %+v, want a one-trial zero-rerun opened by failure #%d", stored, failure.Seq)
	}
	if diff := cmp.Diff(a.Trial.Requirement, stored); diff != "" {
		t.Fatalf("attached requirement differs from the fold's (-attached +stored):\n%s", diff)
	}
	if diff := cmp.Diff(TrialRequirement{Trial: 1, Needed: 1}, h.s.Requirement(p)); diff != "" {
		t.Fatalf("all-zero rerun requirement (-want +got):\n%s", diff)
	}
	h.add(&journal.TrialEnd{Trial: p.Trial, Outcome: journal.OutcomePass})
	if diff := cmp.Diff(TrialRequirement{Trial: 1, Needed: 1}, h.s.TrialHistory(p.Trial).Requirement); diff != "" {
		t.Fatalf("closed all-zero rerun history (-want +got):\n%s", diff)
	}
}

// TestRequirementChecksHuntAndRoundIdentity gives an intent that names a hunt or round other than the open one no
// hunt or deepening requirement: the open hunt's or round's window and count were not what it was scheduled for.
func TestRequirementChecksHuntAndRoundIdentity(t *testing.T) {
	t.Run("hunt", func(t *testing.T) {
		h := huntHarness(t, 4, 120)
		open := h.s.hunt.start.Hunt
		p := h.start(nextTrial(h)).Data.(*journal.TrialIntent)
		if p.Hunt != open || h.s.StoredRequirement(p.Trial).Kind != "hunt" {
			t.Fatalf("the open hunt's own trial %+v stored %+v", p, h.s.StoredRequirement(p.Trial))
		}
		other := *p
		other.Trial, other.Hunt = "other", open+1
		h.add(&other)
		if got := h.s.StoredRequirement("other"); got.Kind != "unclassified" || got.Needed != 1 {
			t.Fatalf("an intent of hunt %d stored %+v while hunt %d is open", other.Hunt, got, open)
		}
	})
	t.Run("round", func(t *testing.T) {
		h := cleanCycleHarness(t, []int{-40, -40, -40, -40}, nil)
		round := nextRound(h)
		a := nextTrial(h)
		if a.Trial.Round != round.Round {
			t.Fatalf("next trial is not of round %d: %+v", round.Round, a)
		}
		p := h.start(a).Data.(*journal.TrialIntent)
		if h.s.StoredRequirement(p.Trial).Kind != "deepening" {
			t.Fatalf("the open round's own trial stored %+v", h.s.StoredRequirement(p.Trial))
		}
		other := *p
		other.Trial, other.Round = "other", round.Round+1
		h.add(&other)
		if got := h.s.StoredRequirement("other"); got.Kind != "unclassified" || got.Needed != 1 {
			t.Fatalf("an intent of round %d stored %+v while round %d is open", other.Round, got, round.Round)
		}
	})
}

// TestRequirementTrialNumberIsBounded never shows a trial past the class's count, however many passes the window holds.
func TestRequirementTrialNumberIsBounded(t *testing.T) {
	cfg := config.Default()
	h := newHarnessOn(t, topology(1), cfg, coreStart{phase: journal.PhaseSearch, offset: -20, check: true})
	needed := cfg.Evidence.Trials()
	for range needed - 1 {
		h.trial(h.next(), passed)
	}
	p := h.start(h.next()).Data.(*journal.TrialIntent)
	for i := range 2 {
		extra := *p
		extra.Trial = fmt.Sprintf("extra%d", i)
		h.add(&extra)
		h.add(&journal.TrialEnd{Trial: extra.Trial, Outcome: journal.OutcomePass})
	}
	if diff := cmp.Diff(TrialRequirement{Passed: needed, Trial: needed, Needed: needed}, h.s.Requirement(p)); diff != "" {
		t.Fatalf("requirement past its count (-want +got):\n%s", diff)
	}
}

// TestCrashRecoveryKeepsTheRunningPart keeps the crashed trial's part running from crash.detected until recovery
// closes the trial with its own trial.end.
func TestCrashRecoveryKeepsTheRunningPart(t *testing.T) {
	h := newHarnessOn(t, topology(8), config.Default(), hasRoomStarts(-10, -20, -30, -40, -20, -20, -20, -20)...)
	h.decide(h.next())
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{machine.R1}})
	p := h.start(nextTrial(h)).Data.(*journal.TrialIntent)
	running := func() bool {
		count := 0
		for _, part := range h.s.CyclePlan().Steps[0].Parts {
			if part.Running {
				count++
			}
		}
		return count == 1
	}
	if !running() {
		t.Fatal("the trial's part is not running")
	}
	h.add(&journal.CrashDetected{PreviousBoot: "b"})
	if !running() {
		t.Fatal("crash.detected dropped the running part before recovery closed the trial")
	}
	h.add(&journal.TrialEnd{Trial: p.Trial, Outcome: journal.OutcomeFailure, Signal: machine.Crash})
	if running() {
		t.Fatal("the part still runs after its trial ended")
	}
}

// nextTrial decides whatever precedes the next trial and returns it.
func nextTrial(h *harness) Action {
	h.t.Helper()
	for range 50 {
		a := h.next()
		if a.Kind == RunTrial {
			return a
		}
		h.decide(a)
	}
	h.t.Fatal("no trial scheduled")
	return Action{}
}
