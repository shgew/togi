package main

import (
	"fmt"
	"reflect"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func computeEvents(events []journal.Event, since time.Time) metrics {
	return compute(facts.FromEvents(events), since)
}

// metricFields compares metrics by their unexported fields, with nil and empty lists alike.
var metricFields = cmp.Options{cmp.Exporter(func(reflect.Type) bool { return true }), cmpopts.EquateEmpty()}

// interruption is a hunt trial cut short by a crash. Its last evidence comes
// ran after it starts, after a wall-clock jump; the next boot logs its first
// event recovery after that evidence, 10 seconds after booting.
type interruption struct {
	hunt     int
	trial    string
	at       time.Time
	ran      time.Duration
	jump     time.Duration
	recovery time.Duration
	evidence journal.Payload
	signal   machine.Signal
}

func interrupted(evidence journal.Payload, signal machine.Signal) interruption {
	return interruption{hunt: 1, trial: "trial", at: time.Unix(100, 0), ran: 20 * time.Second, recovery: 80 * time.Second, evidence: evidence, signal: signal}
}

// events journals the interruption from seq first, in boots named after the trial.
func (c interruption) events(first int) []journal.Event {
	const bootMono = 10000
	crashed, next := c.trial+"/a", c.trial+"/b"
	evidenceAt := c.at.Add(c.ran + c.jump)
	recovered := evidenceAt.Add(c.recovery)
	events := []journal.Event{
		{Boot: crashed, Time: c.at, Mono: bootMono, Data: &journal.HuntStart{Hunt: c.hunt, Trial: "original"}},
		{Boot: crashed, Time: c.at, Mono: bootMono, Data: &journal.TrialIntent{Trial: c.trial, Hunt: c.hunt, Phase: journal.PhaseHunt, Regime: machine.R7, Workload: "load"}},
		{Boot: crashed, Time: c.at, Mono: bootMono, Data: &journal.TrialStart{Trial: c.trial}},
		{Boot: crashed, Time: evidenceAt, Mono: bootMono + c.ran.Milliseconds(), Data: c.evidence},
		{Boot: next, Time: recovered, Mono: bootMono, Data: &journal.ConfigLoaded{}},
		{Boot: next, Time: recovered, Mono: bootMono, Data: &journal.CrashDetected{PreviousBoot: crashed, InFlight: new(first + 1)}},
		{Boot: next, Time: recovered, Mono: bootMono, Data: &journal.TrialEnd{Trial: c.trial, Outcome: journal.OutcomeFailure, Signal: c.signal, DurationS: int(c.ran / time.Second)}},
		{Boot: next, Time: recovered, Mono: bootMono, Data: &journal.Failure{Trial: c.trial, Signal: c.signal, Regime: machine.R7, Attribution: journal.Unattributed}},
		{Boot: next, Time: recovered, Mono: bootMono, Data: &journal.HuntEnd{Hunt: c.hunt, Result: "combination"}},
	}
	for i := range events {
		events[i].Seq = first + i
	}
	return events
}

func TestRecoveryGapUsesLastEvidenceAfterWallJump(t *testing.T) {
	for _, jump := range []time.Duration{-time.Hour, time.Hour} {
		for _, evidence := range []journal.Payload{&journal.TrialProgress{Trial: "trial"}, &journal.TrialSignal{Trial: "trial"}, &journal.TrialSample{Trial: "trial"}} {
			t.Run(fmt.Sprintf("%s/%s", jump, evidence.Kind()), func(t *testing.T) {
				c := interrupted(evidence, machine.Crash)
				c.jump = jump
				m := computeEvents(c.events(1), time.Time{})
				if diff := cmp.Diff(recoveryGap{1, 80, 80, 80}, m.recovery, metricFields); diff != "" {
					t.Errorf("recovery gap (-want +got):\n%s", diff)
				}
				if diff := cmp.Diff(70.0, m.trialTime.downtimeS); diff != "" {
					t.Errorf("downtime (-want +got):\n%s", diff)
				}
			})
		}
	}
}

func TestCrashMetricsPreserveStrongerSignal(t *testing.T) {
	for _, signal := range []machine.Signal{machine.ComputationError, machine.Stall, machine.CorrectedMCE, machine.UncorrectedMCE} {
		t.Run(string(signal), func(t *testing.T) {
			m := computeEvents(interrupted(&journal.TrialProgress{Trial: "trial", Signal: signal}, signal).events(1), time.Time{})
			if diff := cmp.Diff(recoveryGap{1, 80, 80, 80}, m.recovery, metricFields); diff != "" {
				t.Errorf("recovery gap (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff(70.0, m.trialTime.downtimeS); diff != "" {
				t.Errorf("downtime (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff(crashTiming{0, 1, 0, 0, 0}, m.failures.timing); diff != "" {
				t.Errorf("crash timing (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff(1, m.hunts[0].crashes); diff != "" {
				t.Errorf("hunt crashes (-want +got):\n%s", diff)
			}
			want := []entry[failureKey, int]{{failureKey{machine.R7, "load", signal, journal.Unattributed}, 1}}
			if diff := cmp.Diff(want, m.failures.counts, metricFields); diff != "" {
				t.Errorf("failure classification (-want +got):\n%s", diff)
			}
		})
	}
}

func TestRerunsGroupRecordedObligation(t *testing.T) {
	var events []journal.Event
	add := func(p journal.Payload, cause ...int) {
		events = append(events, journal.Event{Seq: len(events) + 1, Boot: "a", Time: time.Unix(int64(len(events)), 0), Cause: cause, Data: p})
	}
	for i, duration := range []int{120, 120, 120, 120, 120, 900, 120} {
		cause := 100
		if i == 6 {
			cause = 200
		}
		id := fmt.Sprint(i)
		add(&journal.TrialIntent{Trial: id, Phase: journal.PhaseChecking, Rerun: true, Regime: machine.R6, DurationS: duration}, cause)
		add(&journal.TrialStart{Trial: id})
		result := journal.OutcomePass
		if i >= 5 {
			result = journal.OutcomeFailure
		}
		add(&journal.TrialEnd{Trial: id, Outcome: result})
	}
	want := checkingMetrics{reruns: 2, rerunsFailing: 1}
	if diff := cmp.Diff(want, computeEvents(events, time.Time{}).checking, metricFields); diff != "" {
		t.Fatal(diff)
	}
}

func TestCheckingStepsListRerunsApart(t *testing.T) {
	var events []journal.Event
	add := func(p journal.Payload, cause ...int) int {
		events = append(events, journal.Event{Seq: len(events) + 1, Boot: "a", Time: time.Unix(int64(len(events)), 0), Cause: cause, Data: p})
		return len(events)
	}
	add(&journal.SessionStart{Session: "reruns", Cores: []machine.CoreInfo{{Core: 0, CCD: 0}, {Core: 1, CCD: 0}}})
	add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart})
	trial := func(id string, cycle int, rerun bool, result journal.Outcome, cause ...int) int {
		add(&journal.TrialIntent{Trial: id, Phase: journal.PhaseChecking, Condition: machine.Together, Regime: machine.R6, Workload: "load", DurationS: 60, Profile: []int{-10, -10}, Cycle: cycle, Rerun: rerun}, cause...)
		add(&journal.TrialStart{Trial: id})
		return add(&journal.TrialEnd{Trial: id, Outcome: result})
	}
	trial("pass", 1, false, journal.OutcomePass)
	failure := trial("fail", 1, false, journal.OutcomeFailure)
	trial("rerun", 0, true, journal.OutcomePass, failure)
	want := []entry[stepKey, int]{
		{stepKey{cycle: 1, regime: machine.R6, cores: "00,01", outcome: "failure"}, 1},
		{stepKey{cycle: 1, regime: machine.R6, cores: "00,01", outcome: "pass"}, 1},
		{stepKey{rerun: true, regime: machine.R6, cores: "00,01", outcome: "pass"}, 1},
	}
	if diff := cmp.Diff(want, computeEvents(events, time.Time{}).checking.steps, metricFields); diff != "" {
		t.Fatalf("checking steps (-want +got):\n%s", diff)
	}
}

func TestHuntCommitmentRequiresRecordedCause(t *testing.T) {
	for _, tc := range []struct {
		name        string
		result      string
		combination bool
		cause       int
		want        string
	}{
		{name: "cancelled unrelated search", result: "cancelled", cause: 99, want: "none"},
		{name: "culprit unrelated search", result: "culprit", cause: 99, want: "none"},
		{name: "culprit end cause", result: "culprit", cause: 2, want: "00 -10->-9"},
		{name: "combination cause", result: "combination", combination: true, cause: 3, want: "00 -10->-9"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			events := []journal.Event{{Seq: 1, Boot: "a", Data: &journal.HuntStart{Hunt: 1}}, {Seq: 2, Boot: "a", Data: &journal.HuntEnd{Hunt: 1, Result: tc.result}}}
			if tc.combination {
				events = append(events, journal.Event{Seq: 3, Boot: "a", Cause: []int{2}, Data: &journal.Combination{Hunt: 1, Combination: 1}})
			}
			events = append(events, journal.Event{Seq: 4, Boot: "a", Cause: []int{tc.cause}, Data: &journal.TunerDecision{Phase: journal.PhaseSearch, Decision: journal.Backoff, Core: 0, FromOffset: -10, ToOffset: -9}})
			if diff := cmp.Diff(tc.want, project(facts.FromEvents(events)).hunts[0].commitment); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestIdleCrashInvalidatesAllCoreR6PriorPasses(t *testing.T) {
	cores := []machine.CoreInfo{{Core: 0}, {Core: 1}}
	for _, tc := range []struct {
		name    string
		regime  machine.Regime
		loaded  []int
		profile []int
		want    int
	}{
		{"legacy absent profile", machine.R6, []int{0, 1}, nil, 1},
		{"equal", machine.R6, []int{0, 1}, []int{-10, -10}, 0},
		{"shallower", machine.R6, []int{0, 1}, []int{-9, -10}, 0},
		{"deeper", machine.R6, []int{0, 1}, []int{-11, -10}, 1},
		{"partial R6", machine.R6, []int{0}, []int{-10, -10}, 1},
		{"R7", machine.R7, []int{0, 1}, []int{-10, -10}, 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			intent := func(id string, hunt int) *journal.TrialIntent {
				return &journal.TrialIntent{Trial: id, Hunt: hunt, Group: hunt, Regime: tc.regime, Cores: tc.loaded, Profile: []int{-10, -10}, Workload: "load", DurationS: 120}
			}
			payloads := []journal.Payload{&journal.SessionStart{Cores: cores}, intent("pass", 0), &journal.TrialEnd{Trial: "pass", Outcome: journal.OutcomePass}, &journal.Failure{Signal: machine.Crash, Profile: tc.profile}, &journal.HuntStart{Hunt: 1, Trials: 1}, &journal.HuntGroup{Hunt: 1, Group: 1, Stage: "part"}, intent("group", 1)}
			var events []journal.Event
			for i, payload := range payloads {
				events = append(events, journal.Event{Seq: i + 1, Boot: "a", Time: time.Unix(int64(i), 0), Data: payload})
			}
			if diff := cmp.Diff(tc.want, computeEvents(events, time.Time{}).evidence.groups[0].prior); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestSingleCarriedFailureDecisions(t *testing.T) {
	at := time.Unix(100, 0)
	for _, tc := range []struct {
		name    string
		payload journal.Payload
		cause   []int
		since   time.Time
		want    int
	}{
		{"inferred failure with hunt context", &journal.HuntGroup{Inferred: "failure"}, []int{5, 1}, time.Time{}, 1},
		{"carried idle failure backoff", &journal.TunerDecision{Decision: journal.Backoff}, []int{2}, time.Time{}, 1},
		{"repeated cause is one observation", &journal.HuntGroup{Inferred: "failure"}, []int{1, 1}, time.Time{}, 1},
		{"two carried failures", &journal.HuntGroup{Inferred: "failure"}, []int{1, 2}, time.Time{}, 0},
		{"carried and live failures", &journal.TunerDecision{Decision: journal.Backoff}, []int{1, 3}, time.Time{}, 0},
		{"live failure only", &journal.TunerDecision{Decision: journal.Backoff}, []int{3}, time.Time{}, 0},
		{"passing inference with context failure", &journal.HuntGroup{Inferred: "pass"}, []int{1, 4, 5}, time.Time{}, 0},
		{"hunt start context citation", &journal.HuntStart{}, []int{1}, time.Time{}, 0},
		{"non-backoff decision", &journal.TunerDecision{Decision: journal.CheckSoloLimit}, []int{1}, time.Time{}, 0},
		{"carried pass is not a failure", &journal.HuntGroup{Inferred: "failure"}, []int{4}, time.Time{}, 0},
		{"decision before cutoff", &journal.HuntGroup{Inferred: "failure"}, []int{1}, at.Add(time.Second), 0},
		{"decision at cutoff uses earlier evidence", &journal.HuntGroup{Inferred: "failure"}, []int{1}, at, 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			events := []journal.Event{
				{Seq: 1, Time: at.Add(-time.Hour), Data: &journal.TrialCarried{Outcome: journal.OutcomeFailure}},
				{Seq: 2, Time: at.Add(-time.Hour), Data: &journal.FailureCarried{}},
				{Seq: 3, Time: at.Add(-time.Hour), Data: &journal.Failure{}},
				{Seq: 4, Time: at.Add(-time.Hour), Data: &journal.TrialCarried{Outcome: journal.OutcomePass}},
				{Seq: 5, Time: at.Add(-time.Hour), Data: &journal.HuntStart{}},
				{Seq: 6, Time: at, Data: tc.payload, Cause: tc.cause},
			}
			if diff := cmp.Diff(tc.want, singleCarriedFailureDecisions(events, tc.since)); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestSingleCarriedFailureThroughHunt(t *testing.T) {
	at := time.Unix(100, 0)
	for _, tc := range []struct {
		name  string
		extra []int
		want  int
	}{
		{"carried failure through combination", nil, 1},
		{"live failure added", []int{3}, 0},
		{"second carried failure added", []int{4}, 0},
		{"repeated failure through multiple paths", []int{1, 5}, 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			events := []journal.Event{
				{Seq: 1, Data: &journal.TrialCarried{Outcome: journal.OutcomeFailure}},
				{Seq: 2, Data: &journal.TrialCarried{Outcome: journal.OutcomePass}},
				{Seq: 3, Data: &journal.TrialEnd{Outcome: journal.OutcomeFailure}},
				{Seq: 4, Data: &journal.FailureCarried{}},
				{Seq: 5, Data: &journal.HuntStart{}, Cause: []int{1}},
				{Seq: 6, Data: &journal.TrialEnd{Outcome: journal.OutcomePass}, Cause: []int{3}},
				{Seq: 7, Data: &journal.HuntEnd{}, Cause: append([]int{5, 2, 6}, tc.extra...)},
				{Seq: 8, Data: &journal.Combination{}, Cause: []int{7}},
				{Seq: 9, Time: at, Data: &journal.TunerDecision{Decision: journal.Backoff}, Cause: []int{8}},
			}
			if diff := cmp.Diff(tc.want, singleCarriedFailureDecisions(events, at)); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}
