package tuner

import (
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
		{"stale cycle duration", machine.R1, func(c *config.Config) { c.Durations.CheckingTrialS++ }, true},
		{"same shape R7 part", machine.R7, func(*config.Config) {}, true},
		// An R7 part ignores a retry whose duration no longer matches the part's and schedules a fresh trial.
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
