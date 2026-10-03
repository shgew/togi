package main

import (
	"fmt"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestReviewBoundaryReport(t *testing.T) {
	at := time.Date(2026, 9, 25, 0, 0, 0, 0, time.UTC)
	var events []journal.Event
	add := func(payload journal.Payload) int {
		events = append(events, journal.Event{Seq: len(events) + 1, Boot: "a", Time: at.Add(time.Duration(len(events)) * time.Minute), Data: payload})
		return len(events)
	}
	add(&journal.SessionStart{Session: "review", Cores: []machine.CoreInfo{{Core: 0, CCD: 0}, {Core: 1, CCD: 1}}})
	add(&journal.ConfigLoaded{Version: "1.2.3"})
	add(&journal.CheckingLap{Lap: 1, Event: journal.LapStart})
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
		add(&journal.TrialIntent{Trial: id, Regime: machine.R6, Workload: workload, DurationS: duration, Profile: profile, Hunt: hunt, Group: group, Condition: condition, Phase: phase, Lap: 1})
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
	parkedSeq := add(&journal.CheckingLap{Lap: 1, Event: journal.LapEnd, Full: true, Passed: true})
	appendTrial("trigger", journal.OutcomeFailure, 0, 0, "load")
	failure := add(&journal.Failure{Trial: "trigger", Regime: machine.R6, Signal: machine.ComputationError, Attribution: journal.Unattributed, Profile: []int{-11, -11}})
	for _, hunt := range []int{1, 2} {
		add(&journal.HuntStart{Hunt: hunt, Trial: "trigger", Failure: failure, ParkedSeq: parkedSeq, Parked: []int{-10, -10}, Failing: []int{-11, -11}, Candidates: []int{0, 1}, Cores: []int{0, 1}, Regime: machine.R6, Workload: "load", DurationS: 120, Starts: 1, StartS: 60})
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
		if diff := cmp.Diff([][]string{{"2", "1", "part", "1", "1", "true", "1", "0", "1", "0.017"}, {"2", "2", "part", "1", "1", "true", "0", "1", "1", "0.017"}, {"2", "3", "part", "1", "1", "true", "0", "0", "1", "0.017"}}, reportRows(t, events, "Prior evidence per hunt group", since)); diff != "" {
			t.Fatal(diff)
		}
		if diff := cmp.Diff([][]string{{"0002", "part", "3", "3", "1", "1", "3", "0.050"}}, reportRows(t, events, "Prior evidence by hunt and stage", since)); diff != "" {
			t.Fatal(diff)
		}
	}
	if diff := cmp.Diff([][]string{{"0001", "R6", "00,01", "failure", "1"}, {"0001", "R6", "00,01", "pass", "1"}}, reportRows(t, events, "Checking steps and together outcomes")); diff != "" {
		t.Fatalf("legacy all-core expansion (-want +got):\n%s", diff)
	}
	rows := reportRows(t, events, "Hunts")
	if diff := cmp.Diff("1/0/0/1", rows[0][6]); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff(fmt.Sprintf("#%d", parkedSeq), rows[0][4]); diff != "" {
		t.Fatal(diff)
	}
	if got := reportRows(t, events, "Hunts", cutoff); len(got) != 1 || got[0][0] != "2" {
		t.Fatalf("cutoff hunts = %v", got)
	}
	if diff := cmp.Diff([][]string{{"inconclusive", "1"}, {"decisions", "resting", "on", "a", "single", "carried", "failure", "0"}}, reportRows(t, events, "Evidence quality", cutoff)); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff([][]string{{"2", "parked", "no", "reliable", "result"}}, reportRows(t, events, "Inconclusive trials")); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff([][]string{{"laps", "started", "0"}, {"laps", "ended", "0"}, {"full", "laps", "0"}, {"reruns", "0"}, {"reruns", "failing", "first", "start", "0"}}, reportRows(t, events, "Checking", cutoff)); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff([][]string{{"R6", "load", "3", "0.050", "1"}}, reportRows(t, events, "Exposure", cutoff)); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff([][]string{{"R6", "load", string(machine.ComputationError), "unattributed", "1"}}, reportRows(t, events, "Failures", cutoff)); diff != "" {
		t.Fatal(diff)
	}
	fields := map[string]string{}
	for _, row := range reportRows(t, events, "Session", cutoff) {
		if row[0] == "builds" || row[0] == "since" {
			fields[row[0]] = row[1]
		}
	}
	if diff := cmp.Diff(map[string]string{"builds": "1.2.3", "since": cutoff.Format(time.RFC3339)}, fields); diff != "" {
		t.Fatal(diff)
	}
}

func TestReviewEvenRecoveryMedian(t *testing.T) {
	var events []journal.Event
	for i, gap := range []int{90, 10, 50, 30} {
		crash := interruptedEvents(0, &journal.TrialProgress{Trial: "trial"}, machine.Crash)
		crash[0].Data = &journal.ConfigLoaded{}
		if i == 0 {
			crash[0].Data = &journal.SessionStart{Session: "review", Cores: []machine.CoreInfo{{Core: 0}, {Core: 1}}}
		}
		intent := crash[1].Data.(*journal.TrialIntent)
		intent.Hunt = 0
		intent.Phase = journal.PhaseChecking
		intent.Condition = machine.Together
		intent.Profile = []int{-10, -10}
		intent.DurationS = 120
		crash[8].Data = &journal.Shutdown{Reason: journal.ShutdownCommand}
		for j := range crash {
			crash[j].Seq += i * 9
			crash[j].Time = crash[j].Time.Add(time.Duration(i) * time.Hour)
			crash[j].Boot = fmt.Sprintf("%d-%s", i, crash[j].Boot)
			if j >= 4 {
				crash[j].Time = crash[j].Time.Add(time.Duration(gap-80) * time.Second)
			}
			switch p := crash[j].Data.(type) {
			case *journal.TrialIntent:
				p.Trial = fmt.Sprint(i)
			case *journal.TrialStart:
				p.Trial = fmt.Sprint(i)
			case *journal.TrialProgress:
				p.Trial = fmt.Sprint(i)
			case *journal.TrialEnd:
				p.Trial = fmt.Sprint(i)
			case *journal.Failure:
				p.Trial = fmt.Sprint(i)
			case *journal.CrashDetected:
				p.PreviousBoot = fmt.Sprintf("%d-a", i)
				p.InFlight = new(i*9 + 2)
			}
		}
		events = append(events, crash...)
	}
	if diff := cmp.Diff([][]string{{"4", "10.000", "40.000", "90.000"}}, reportRows(t, events, "Recovery gap")); diff != "" {
		t.Fatal(diff)
	}
}

func TestCrashTimingBoundaries(t *testing.T) {
	for _, duration := range []int{30, 59, 60, 119} {
		t.Run(fmt.Sprint(duration), func(t *testing.T) {
			events := interruptedEvents(0, &journal.TrialProgress{Trial: "trial"}, machine.Crash)
			events[3].Time = events[3].Time.Add(time.Duration(duration-20) * time.Second)
			events[3].Mono += int64(duration-20) * 1000
			events[6].Data.(*journal.TrialEnd).DurationS = duration
			for i := 4; i < len(events); i++ {
				events[i].Time = events[i].Time.Add(time.Duration(duration-20) * time.Second)
			}
			events[0].Data = &journal.SessionStart{Session: "review", Cores: []machine.CoreInfo{{Core: 0}, {Core: 1}}}
			intent := events[1].Data.(*journal.TrialIntent)
			intent.Hunt = 0
			intent.Phase = journal.PhaseChecking
			intent.Condition = machine.Together
			intent.Profile = []int{-10, -10}
			intent.DurationS = 120
			events[8].Data = &journal.Shutdown{Reason: journal.ShutdownCommand}
			want := [][]string{{"0", "0"}, {"1-29", "0"}, {"30-59", "0"}, {"60-119", "0"}, {"120+", "0"}}
			bin := 2
			if duration >= 60 {
				bin = 3
			}
			want[bin][1] = "1"
			if diff := cmp.Diff(want, reportRows(t, events, "Crash timing (last evidence)")); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}
