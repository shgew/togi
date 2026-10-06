package main

import (
	"errors"
	"fmt"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

// failingFirstWrite fails its first write and accepts every later one.
type failingFirstWrite struct{ failed bool }

var errFirstWrite = errors.New("first write failed")

func (w *failingFirstWrite) Write(p []byte) (int, error) {
	if !w.failed {
		w.failed = true
		return 0, errFirstWrite
	}
	return len(p), nil
}

func TestReportReturnsEarlierSectionWriteError(t *testing.T) {
	if err := report(&failingFirstWrite{}, facts.FromEvents(nil), time.Time{}); !errors.Is(err, errFirstWrite) {
		t.Fatalf("report error = %v, want %v", err, errFirstWrite)
	}
}

func TestReviewBoundaryReport(t *testing.T) {
	at := time.Date(2026, 9, 25, 0, 0, 0, 0, time.UTC)
	var events []journal.Event
	add := func(payload journal.Payload) int {
		events = append(events, journal.Event{Seq: len(events) + 1, Boot: "a", Time: at.Add(time.Duration(len(events)) * time.Minute), Data: payload})
		return len(events)
	}
	add(&journal.SessionStart{Session: "review", Cores: []machine.CoreInfo{{Core: 0, CCD: 0}, {Core: 1, CCD: 1}}})
	add(&journal.ConfigLoaded{Version: "1.2.3"})
	add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart})
	appendTrial := func(id string, result journal.Outcome, hunt, group int, workload string) {
		condition, phase := machine.Together, journal.PhaseChecking
		if hunt != 0 {
			condition, phase = machine.Parked, journal.PhaseHunt
		}
		duration := 60
		profile := []int{-10, -10}
		if id == "trigger" {
			duration = 120
			profile = []int{-11, -11}
		}
		add(&journal.TrialIntent{Trial: id, Regime: machine.R6, Workload: workload, DurationS: duration, Profile: profile, Hunt: hunt, Group: group, Condition: condition, Phase: phase, Cycle: 1})
		add(&journal.TrialStart{Trial: id})
		if duration == 120 {
			add(&journal.TrialProgress{Trial: id})
		}
		end := &journal.TrialEnd{Trial: id, Outcome: result, DurationS: duration}
		if result == journal.OutcomeFailure {
			end.Signal = machine.ComputationError
		}
		if result == journal.OutcomeInconclusive {
			end.Reason = "no reliable result"
		}
		add(end)
	}
	appendTrial("prior", journal.OutcomePass, 0, 0, "load")
	parkedSeq := add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleEnd, Full: true, Passed: true})
	appendTrial("trigger", journal.OutcomeFailure, 0, 0, "load")
	failure := add(&journal.Failure{Trial: "trigger", Regime: machine.R6, Signal: machine.ComputationError, Attribution: journal.Unattributed, Profile: []int{-11, -11}})
	for _, hunt := range []int{1, 2} {
		add(&journal.HuntStart{Hunt: hunt, Trial: "trigger", Failure: failure, ParkedSeq: parkedSeq, Parked: []int{-10, -10}, Failing: []int{-11, -11}, Candidates: []int{0, 1}, Cores: []int{0, 1}, Regime: machine.R6, Workload: "load", DurationS: 120, Trials: 1, TrialS: 60})
		if hunt == 1 {
			add(&journal.HuntGroup{Hunt: 1, Group: 1, Skipped: true, Stage: "part", Cores: []int{0, 1}})
			add(&journal.HuntEnd{Hunt: 1, Result: "cancelled"})
		}
	}
	cutoff := events[len(events)-1].Time
	add(&journal.TunerWarning{Warning: "inconclusive"})
	for i, result := range []journal.Outcome{journal.OutcomePass, journal.OutcomeFailure, journal.OutcomeInconclusive} {
		add(&journal.HuntGroup{Hunt: 2, Group: i + 1, Stage: "part", Cores: []int{0, 1}, Profile: []int{-10, -10}, DurationS: 60})
		appendTrial(fmt.Sprint(i), result, 2, i+1, "load")
		if result == journal.OutcomeFailure {
			add(&journal.Failure{Trial: fmt.Sprint(i), Signal: machine.ComputationError, Attribution: journal.Unattributed, Profile: []int{-10, -10}})
		}
	}
	add(&journal.HuntEnd{Hunt: 2, Result: "combination", Members: []journal.CombinationMember{{Core: 1, Offset: -10}, {Core: 0, Offset: -10}}})
	for _, since := range []time.Time{{}, cutoff} {
		m := computeEvents(events, since)
		wantGroups := []groupReuse{
			{hunt: 2, group: 1, stage: "part", prior: 1, required: 1, established: true, passes: 1, trials: 1, seconds: 60},
			{hunt: 2, group: 2, stage: "part", prior: 1, required: 1, established: true, failures: 1, trials: 1, seconds: 60},
			{hunt: 2, group: 3, stage: "part", prior: 1, required: 1, established: true, trials: 1, seconds: 60},
		}
		if diff := cmp.Diff(wantGroups, m.evidence.groups, metricFields); diff != "" {
			t.Fatal(diff)
		}
		wantStages := []entry[stageKey, stageReuse]{{stageKey{2, "part"}, stageReuse{groups: 3, established: 3, passed: 1, failed: 1, trials: 3, seconds: 180}}}
		if diff := cmp.Diff(wantStages, m.evidence.stages, metricFields); diff != "" {
			t.Fatal(diff)
		}
	}
	all, recent := computeEvents(events, time.Time{}), computeEvents(events, cutoff)
	wantSteps := []entry[stepKey, int]{
		{stepKey{cycle: 1, regime: machine.R6, cores: "00,01", outcome: "failure"}, 1},
		{stepKey{cycle: 1, regime: machine.R6, cores: "00,01", outcome: "pass"}, 1},
	}
	if diff := cmp.Diff(wantSteps, all.checking.steps, metricFields); diff != "" {
		t.Fatalf("legacy all-core expansion (-want +got):\n%s", diff)
	}
	cancelled := all.hunts[0]
	if diff := cmp.Diff([4]int{1, 0, 0, 1}, [4]int{len(cancelled.groups), cancelled.ran, cancelled.inferred, cancelled.skipped}); diff != "" {
		t.Fatalf("planned/run/inferred/skipped (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff(parkedSeq, cancelled.start.ParkedSeq); diff != "" {
		t.Fatal(diff)
	}
	if len(recent.hunts) != 1 || recent.hunts[0].start.Hunt != 2 {
		t.Fatalf("cutoff hunts = %v", recent.hunts)
	}
	if diff := cmp.Diff([]entry[string, int]{{"inconclusive", 1}}, recent.evidence.warnings, metricFields); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff(0, recent.evidence.singleCarried); diff != "" {
		t.Fatal(diff)
	}
	if len(all.inconclusive) != 1 || all.inconclusive[0].Intent.Trial != "2" || all.inconclusive[0].Intent.Condition != machine.Parked || all.inconclusive[0].End.Reason != "no reliable result" {
		t.Fatalf("inconclusive trials = %v", all.inconclusive)
	}
	if diff := cmp.Diff(checkingMetrics{}, recent.checking, metricFields); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff([]entry[exposureKey, exposure]{{exposureKey{machine.R6, "load"}, exposure{trials: 3, seconds: 180, failures: 1}}}, recent.exposure, metricFields); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff([]entry[failureKey, int]{{failureKey{machine.R6, "load", machine.ComputationError, journal.Unattributed}, 1}}, recent.failures.counts, metricFields); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff([]string{"1.2.3"}, recent.session.builds); diff != "" {
		t.Fatal(diff)
	}
}

func TestReviewEvenRecoveryMedian(t *testing.T) {
	var events []journal.Event
	for i, gap := range []int{90, 10, 50, 30} {
		id := fmt.Sprint(i)
		c := interrupted(&journal.TrialProgress{Trial: id}, machine.Crash)
		c.hunt, c.trial = i+1, id
		c.at = c.at.Add(time.Duration(i) * time.Hour)
		c.recovery = time.Duration(gap) * time.Second
		events = append(events, c.events(len(events)+1)...)
	}
	if diff := cmp.Diff(recoveryGap{4, 10, 40, 90}, computeEvents(events, time.Time{}).recovery, metricFields); diff != "" {
		t.Fatal(diff)
	}
}

func TestCrashTimingBoundaries(t *testing.T) {
	for _, duration := range []int{30, 59, 60, 119} {
		t.Run(fmt.Sprint(duration), func(t *testing.T) {
			c := interrupted(&journal.TrialProgress{Trial: "trial"}, machine.Crash)
			c.ran = time.Duration(duration) * time.Second
			events := c.events(1)
			var want crashTiming
			bin := 2
			if duration >= 60 {
				bin = 3
			}
			want[bin] = 1
			if diff := cmp.Diff(want, computeEvents(events, time.Time{}).failures.timing); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}
