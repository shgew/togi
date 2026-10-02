package main

import (
	"bytes"
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func reportRows(t *testing.T, events []journal.Event, section string, cutoff ...time.Time) [][]string {
	t.Helper()
	var out bytes.Buffer
	var since time.Time
	if len(cutoff) > 0 {
		since = cutoff[0]
	}
	if err := report(&out, facts.FromEvents(events), since); err != nil {
		t.Fatal(err)
	}
	for block := range strings.SplitSeq(out.String(), "\n\n") {
		lines := strings.Split(strings.TrimSpace(block), "\n")
		if lines[0] != section {
			continue
		}
		var rows [][]string
		for _, line := range lines[2:] {
			rows = append(rows, strings.Fields(line))
		}
		return rows
	}
	t.Fatalf("missing section %q", section)
	return nil
}

func interruptedEvents(jump time.Duration, evidence journal.Payload, signal machine.Signal) []journal.Event {
	at := time.Unix(100, 0)
	return []journal.Event{
		{Seq: 1, Boot: "a", Time: at, Mono: 10000, Data: &journal.HuntStart{Hunt: 1, Trial: "original"}},
		{Seq: 2, Boot: "a", Time: at, Mono: 10000, Data: &journal.TrialIntent{Trial: "trial", Hunt: 1, Phase: journal.PhaseHunt, Regime: machine.R7, Workload: "load"}},
		{Seq: 3, Boot: "a", Time: at, Mono: 10000, Data: &journal.TrialStart{Trial: "trial"}},
		{Seq: 4, Boot: "a", Time: at.Add(20*time.Second + jump), Mono: 30000, Data: evidence},
		{Seq: 5, Boot: "b", Time: at.Add(100*time.Second + jump), Mono: 10000, Data: &journal.ConfigLoaded{}},
		{Seq: 6, Boot: "b", Time: at.Add(100*time.Second + jump), Mono: 10000, Data: &journal.CrashDetected{PreviousBoot: "a", InFlight: new(2)}},
		{Seq: 7, Boot: "b", Time: at.Add(100*time.Second + jump), Mono: 10000, Data: &journal.TrialEnd{Trial: "trial", Outcome: journal.OutcomeFailure, Signal: signal, DurationS: 20}},
		{Seq: 8, Boot: "b", Time: at.Add(100*time.Second + jump), Mono: 10000, Data: &journal.Failure{Trial: "trial", Signal: signal, Regime: machine.R7, Attribution: journal.Unattributed}},
		{Seq: 9, Boot: "b", Time: at.Add(100*time.Second + jump), Mono: 10000, Data: &journal.HuntEnd{Hunt: 1, Result: "joint"}},
	}
}

func TestRecoveryGapUsesLastEvidenceAfterWallJump(t *testing.T) {
	for _, jump := range []time.Duration{-time.Hour, time.Hour} {
		for _, evidence := range []journal.Payload{&journal.TrialProgress{Trial: "trial"}, &journal.TrialSignal{Trial: "trial"}, &journal.TrialSample{Trial: "trial"}} {
			t.Run(fmt.Sprintf("%s/%s", jump, evidence.Kind()), func(t *testing.T) {
				events := interruptedEvents(jump, evidence, machine.Crash)
				if diff := cmp.Diff([][]string{{"1", "80.000", "80.000", "80.000"}}, reportRows(t, events, "Recovery gap")); diff != "" {
					t.Errorf("recovery gap (-want +got):\n%s", diff)
				}
				rows := reportRows(t, events, "Time")
				if diff := cmp.Diff([]string{"crash", "downtime", "(last", "evidence", "to", "next", "boot)", "70.000", "0.019"}, rows[len(rows)-1]); diff != "" {
					t.Errorf("downtime (-want +got):\n%s", diff)
				}
			})
		}
	}
}

func TestCrashMetricsPreserveStrongerSignal(t *testing.T) {
	for _, signal := range []machine.Signal{machine.ComputationError, machine.Stall, machine.CorrectedMCE, machine.UncorrectedMCE} {
		t.Run(string(signal), func(t *testing.T) {
			events := interruptedEvents(0, &journal.TrialProgress{Trial: "trial", Signal: signal}, signal)
			if diff := cmp.Diff([][]string{{"1", "80.000", "80.000", "80.000"}}, reportRows(t, events, "Recovery gap")); diff != "" {
				t.Errorf("recovery gap (-want +got):\n%s", diff)
			}
			rows := reportRows(t, events, "Time")
			if diff := cmp.Diff("70.000", rows[len(rows)-1][7]); diff != "" {
				t.Errorf("downtime (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff([][]string{{"0", "0"}, {"1-29", "1"}, {"30-59", "0"}, {"60-119", "0"}, {"120+", "0"}}, reportRows(t, events, "Crash timing (last evidence)")); diff != "" {
				t.Errorf("crash timing (-want +got):\n%s", diff)
			}
			hunts := reportRows(t, events, "Hunts")
			if diff := cmp.Diff("1", hunts[0][8]); diff != "" {
				t.Errorf("hunt crashes (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff([][]string{{"R7", "load", string(signal), "unattributed", "1"}}, reportRows(t, events, "Failures")); diff != "" {
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
		add(&journal.TrialIntent{Trial: id, Phase: journal.PhaseGuard, Rerun: true, Regime: machine.R6, DurationS: duration}, cause)
		add(&journal.TrialStart{Trial: id})
		result := journal.OutcomePass
		if i >= 5 {
			result = journal.OutcomeFailure
		}
		add(&journal.TrialEnd{Trial: id, Outcome: result})
	}
	want := [][]string{{"rotations", "started", "0"}, {"rotations", "ended", "0"}, {"rotations", "qualifying", "0"}, {"reruns", "2"}, {"reruns", "failing", "first", "start", "1"}}
	if diff := cmp.Diff(want, reportRows(t, events, "Guard")); diff != "" {
		t.Fatal(diff)
	}
}

func TestHuntCommitmentRequiresRecordedCause(t *testing.T) {
	for _, tc := range []struct {
		name   string
		result string
		joint  bool
		cause  int
		want   string
	}{
		{name: "cancelled unrelated search", result: "cancelled", cause: 99, want: "none"},
		{name: "culprit unrelated search", result: "culprit", cause: 99, want: "none"},
		{name: "culprit end cause", result: "culprit", cause: 2, want: "00 -10->-9"},
		{name: "joint mark cause", result: "joint", joint: true, cause: 3, want: "00 -10->-9"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			events := []journal.Event{{Seq: 1, Boot: "a", Data: &journal.HuntStart{Hunt: 1}}, {Seq: 2, Boot: "a", Data: &journal.HuntEnd{Hunt: 1, Result: tc.result}}}
			if tc.joint {
				events = append(events, journal.Event{Seq: 3, Boot: "a", Cause: []int{2}, Data: &journal.MarkJoint{Hunt: 1, Mark: 1}})
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
		want    string
	}{
		{"legacy absent profile", machine.R6, []int{0, 1}, nil, "1"},
		{"equal", machine.R6, []int{0, 1}, []int{-10, -10}, "0"},
		{"shallower", machine.R6, []int{0, 1}, []int{-9, -10}, "0"},
		{"deeper", machine.R6, []int{0, 1}, []int{-11, -10}, "1"},
		{"partial R6", machine.R6, []int{0}, []int{-10, -10}, "1"},
		{"R7", machine.R7, []int{0, 1}, []int{-10, -10}, "1"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			intent := func(id string, hunt int) *journal.TrialIntent {
				return &journal.TrialIntent{Trial: id, Hunt: hunt, Mask: hunt, Regime: tc.regime, Cores: tc.loaded, Profile: []int{-10, -10}, Workload: "load", DurationS: 120}
			}
			payloads := []journal.Payload{&journal.SessionStart{Cores: cores}, intent("pass", 0), &journal.TrialEnd{Trial: "pass", Outcome: journal.OutcomePass}, &journal.Failure{Signal: machine.Crash, Profile: tc.profile}, &journal.HuntStart{Hunt: 1, Starts: 1}, &journal.HuntMask{Hunt: 1, Mask: 1, Stage: "part"}, intent("mask", 1)}
			var events []journal.Event
			for i, payload := range payloads {
				events = append(events, journal.Event{Seq: i + 1, Boot: "a", Time: time.Unix(int64(i), 0), Data: payload})
			}
			rows := reportRows(t, events, "Prior evidence per hunt mask")
			if diff := cmp.Diff(tc.want, rows[0][3]); diff != "" {
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
		{"inferred failure with hunt context", &journal.HuntMask{Inferred: "failure"}, []int{5, 1}, time.Time{}, 1},
		{"carried idle failure backoff", &journal.TunerDecision{Decision: journal.Backoff}, []int{2}, time.Time{}, 1},
		{"repeated cause is one observation", &journal.HuntMask{Inferred: "failure"}, []int{1, 1}, time.Time{}, 1},
		{"two carried failures", &journal.HuntMask{Inferred: "failure"}, []int{1, 2}, time.Time{}, 0},
		{"carried and live failures", &journal.TunerDecision{Decision: journal.Backoff}, []int{1, 3}, time.Time{}, 0},
		{"live failure only", &journal.TunerDecision{Decision: journal.Backoff}, []int{3}, time.Time{}, 0},
		{"passing inference with context failure", &journal.HuntMask{Inferred: "pass"}, []int{1, 4, 5}, time.Time{}, 0},
		{"hunt start context citation", &journal.HuntStart{}, []int{1}, time.Time{}, 0},
		{"non-backoff decision", &journal.TunerDecision{Decision: journal.CheckEdge}, []int{1}, time.Time{}, 0},
		{"carried pass is not a failure", &journal.HuntMask{Inferred: "failure"}, []int{4}, time.Time{}, 0},
		{"decision before cutoff", &journal.HuntMask{Inferred: "failure"}, []int{1}, at.Add(time.Second), 0},
		{"decision at cutoff uses earlier evidence", &journal.HuntMask{Inferred: "failure"}, []int{1}, at, 1},
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
