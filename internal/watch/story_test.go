package watch

import (
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func storyConfig(steps []machine.Regime, startS int) *journal.ConfigLoaded {
	c := config.Default()
	if steps == nil {
		steps = c.Checking.Lap
	}
	return &journal.ConfigLoaded{Config: journal.ConfigSnapshot{
		Durations: journal.ConfigDurations{SearchTrialS: c.Durations.SearchTrialS, StartS: startS, CheckingTrialS: c.Durations.CheckingTrialS, CheckingIdleS: c.Durations.CheckingIdleS, CheckingAllCoreS: c.Durations.CheckingAllCoreS},
		Evidence:  journal.ConfigEvidence{Miss: c.Evidence.Miss, Rate: c.Evidence.Rate},
		Checking:  journal.ConfigChecking{Lap: steps},
	}}
}

func appendStoryEvents(events []journal.Event, payloads ...journal.Payload) []journal.Event {
	for _, p := range payloads {
		i := len(events)
		events = append(events, journal.Event{Seq: i + 1, Time: time.Unix(1000+int64(i), 0).UTC(), Boot: "boot", Kind: p.Kind(), Msg: p.Message(), Data: p})
	}
	return events
}

func storyText(s Snapshot) string {
	return strings.Join(s.story(time.Unix(2000, 0).UTC()).paragraphs, "\n")
}

func TestStoryIntentWaitsForActualStart(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name      string
		condition machine.Condition
		phase     journal.Phase
		round     int
		rerun     bool
	}{
		{"search", machine.Alone, journal.PhaseSearch, 0, false},
		{"hunt", machine.Parked, journal.PhaseHunt, 0, false},
		{"deepening", machine.Together, journal.PhaseDeepening, 2, false},
		{"rerun", machine.Together, journal.PhaseChecking, 0, true},
		{"checking", machine.Together, journal.PhaseChecking, 0, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			intent := &journal.TrialIntent{Trial: "planned", Core: new(0), Offset: new(-25), Condition: tc.condition, Phase: tc.phase, Regime: machine.R1, Workload: "mprime-sse-4k-21k", DurationS: 120, Profile: []int{-25, 0, 0}, Round: tc.round, Rerun: tc.rerun}
			events := dashboardEvents(dashboardSession(), intent, &journal.SMUIntent{Op: journal.SMUSet, Core: new(0), Offset: -25})
			for _, readback := range []bool{false, true} {
				if readback {
					events = appendStoryEvents(events, &journal.SMUReadback{Core: 0, Offset: -25})
				}
				s := Project(events)
				st := s.story(time.Unix(2000, 0).UTC())
				if text := strings.Join(st.paragraphs, "\n"); !strings.Contains(text, "I'm preparing") || !strings.Contains(text, "hasn't started yet") {
					t.Fatalf("intent described as running (readback=%t): %s", readback, text)
				}
				if st.now == nil || st.now.timed || !strings.HasPrefix(st.now.what, "preparing ") || !strings.HasPrefix(st.now.detail, "planned: ") {
					t.Fatalf("intent shows running timer or load: %+v", st.now)
				}
			}
			events = appendStoryEvents(events, &journal.TrialStart{Trial: intent.Trial})
			st := Project(events).story(events[len(events)-1].Time.Add(time.Second))
			if strings.Contains(strings.Join(st.paragraphs, "\n"), "hasn't started yet") || st.now == nil || !st.now.timed {
				t.Fatalf("actual start still narrated as preparation: %+v", st)
			}
		})
	}
}

func TestStoryDeepeningUsesTogetherScheduledChecks(t *testing.T) {
	t.Parallel()
	events := dashboardEvents(dashboardSession(),
		&journal.CorePhase{Core: 0, To: journal.PhaseHasRoom, Offset: -20},
		&journal.CorePhase{Core: 1, To: journal.PhaseAtLimit, Offset: -30, FailurePoint: new(-31)},
		&journal.CorePhase{Core: 2, To: journal.PhaseAtLimit, Offset: -10, FailurePoint: new(-11)},
		&journal.DeepeningRound{Round: 2, Event: journal.LapStart, Base: []int{-20, -30, -10}, Target: []int{-22, -29, -10}, Profile: []int{-21, -29, -10}, Cores: []int{0, 1}, Starts: 3, StartS: 120},
		&journal.TunerDecision{Core: 1, Phase: journal.PhaseDeepening, Decision: journal.Yield, FromOffset: -30, ToOffset: -29, FailurePoint: new(-31)},
		&journal.TunerDecision{Core: 0, Phase: journal.PhaseDeepening, Decision: journal.Deepen, FromOffset: -20, ToOffset: -21},
		&journal.TrialIntent{Trial: "deepening", Condition: machine.Together, Phase: journal.PhaseDeepening, Regime: machine.R1, Workload: "mprime-sse-24k-160k", Core: new(0), Offset: new(-21), Profile: []int{-21, -29, -10}, DurationS: 120, Round: 2},
		&journal.TrialStart{Trial: "deepening"})
	s := Project(events)
	if s.deepening == nil || len(s.deepening.Checks) == 0 {
		t.Fatal("fixture has no projected deepening checks")
	}
	text := storyText(s)
	for _, want := range []string{"passed a full lap", "core 00 goes deeper to -21", "core 01 yields to -29", "whole proposed profile stays applied", "3 light load runs on core 00", "3 heavy vector load runs on core 00"} {
		if !strings.Contains(text, want) {
			t.Fatalf("missing %q: %s", want, text)
		}
	}
	for _, wrong := range []string{"passed a clean lap", "runs alone", "light load runs on core 01", "heavy vector load runs on core 01"} {
		if strings.Contains(text, wrong) {
			t.Fatalf("promised unplanned check %q: %s", wrong, text)
		}
	}
	next := strings.Join(s.comingUp(), "\n")
	if !strings.Contains(next, "keep deepening while more depth is reachable") || !strings.Contains(next, "return to checking laps") || strings.Contains(next, "new clean lap") {
		t.Fatalf("passed round promises the wrong continuation: %s", next)
	}
	for _, st := range s.stations() {
		if st.label == "Test together" && strings.Contains(strings.Join(st.sub, " "), "clean") {
			t.Fatalf("passed full lap incorrectly described as clean: %+v", st)
		}
	}
}

func TestStoryMemberProbeNamesCombinationNotLoadedCores(t *testing.T) {
	t.Parallel()
	for _, loaded := range [][]int{{1}, {0, 1, 2}} {
		events := dashboardEvents(dashboardSession(),
			&journal.CorePhase{Core: 0, To: journal.PhaseHasRoom, Offset: -20},
			&journal.CorePhase{Core: 1, To: journal.PhaseAtLimit, Offset: -30},
			&journal.CorePhase{Core: 2, To: journal.PhaseHasRoom, Offset: -10},
			&journal.HuntStart{Hunt: 1, Regime: machine.R7, Workload: "mprime-avx2-36k-248k-allcore", Cores: loaded, Parked: []int{0, 0, 0}, Failing: []int{-20, -30, -10}, Candidates: []int{0, 2}, Starts: 5, StartS: 120, DurationS: 120},
			&journal.HuntGroup{Hunt: 1, Group: 1, Stage: "probe", Cores: []int{2}, Probe: &journal.CombinationMember{Core: 0, Offset: -15}, Profile: []int{-15, 0, -10}, DurationS: 120},
			&journal.TrialIntent{Trial: "probe", Condition: machine.Parked, Phase: journal.PhaseHunt, Regime: machine.R7, Workload: "mprime-avx2-36k-248k-allcore", Cores: loaded, Profile: []int{-15, 0, -10}, DurationS: 120, Hunt: 1, Group: 1},
			&journal.TrialStart{Trial: "probe"})
		if text := storyText(Project(events)); !strings.Contains(text, "Cores 00, 02 fail only together") || strings.Contains(text, "core 01 must back off") {
			t.Fatalf("probe combination confused with loaded cores %v: %s", loaded, text)
		}
	}
}

func TestStoryDeadEndsPreserveCauseWithoutInventingDiagnosis(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		condition journal.DeadEndCondition
		detail    string
		wrong     string
	}{
		{journal.DeadEndNoEvidence, "kernel log of boot previous unreadable after retries at 1, 5 and 30 min: permission denied", "installed"},
		{journal.DeadEndNoEvidence, "backend mprime inconclusive 8 times in a row after retries at 1, 5 and 30 min", "kept failing to run"},
		{journal.DeadEndContainment, "cleanup togi-trial.scope: termination unconfirmed", "ran outside"},
		{journal.DeadEndContainment, "unexpected CPU outside allowed cpuset", "termination confirmed"},
	} {
		d := &journal.DeadEnd{Condition: tc.condition, Detail: tc.detail}
		text := storyText(Project(dashboardEvents(dashboardSession(), d)))
		if !strings.Contains(text, d.Message()) || strings.Contains(text, tc.wrong) {
			t.Fatalf("lost recorded cause or invented diagnosis: %s", text)
		}
	}
}

func TestStoryPartialCheckingScheduleCannotPromiseGoal(t *testing.T) {
	t.Parallel()
	cfg := storyConfig([]machine.Regime{machine.R1}, 120)
	search := Project(dashboardEvents(dashboardSession(), cfg,
		&journal.CorePhase{Core: 0, To: journal.PhaseSearch, Offset: -5}))
	if next := strings.Join(search.comingUp(), "\n"); strings.Contains(next, "laps of every kind") || !strings.Contains(next, "cannot count") || !strings.Contains(next, "R2:") {
		t.Fatalf("search promises unavailable coverage: %s", next)
	}
	events := dashboardEvents(dashboardSession(), cfg,
		&journal.CorePhase{Core: 0, To: journal.PhaseAtLimit, Offset: -50},
		&journal.CorePhase{Core: 1, To: journal.PhaseAtLimit, Offset: -50},
		&journal.CorePhase{Core: 2, To: journal.PhaseAtLimit, Offset: -50},
		&journal.ProfileChange{To: []int{-50, -50, -50}},
		&journal.CheckingLap{Lap: 1, Event: journal.LapStart, Steps: []machine.Regime{machine.R1}},
		&journal.TrialIntent{Trial: "partial", Condition: machine.Together, Phase: journal.PhaseChecking, Regime: machine.R1, Workload: "mprime-sse-4k-21k", Core: new(0), Offset: new(-50), Profile: []int{-50, -50, -50}, DurationS: 120},
		&journal.TrialStart{Trial: "partial"})
	s := Project(events)
	if s.checking == nil || s.checking.Full || len(s.checking.Missing) == 0 {
		t.Fatal("fixture did not project a partial schedule")
	}
	for _, text := range []string{storyText(s), strings.Join(s.comingUp(), "\n")} {
		if strings.Contains(text, "covers every kind of load") || strings.Contains(text, "that's the goal") || !strings.Contains(text, "cannot count") || !strings.Contains(text, "R2:") {
			t.Fatalf("partial schedule promises full clean-lap goal: %s", text)
		}
	}
	for _, st := range s.stations() {
		if st.label == "Clean lap" && (st.state == target || st.state == reached || strings.Join(st.sub, " ") != "not covered by schedule") {
			t.Fatalf("partial schedule shows achievable goal: %+v", st)
		}
	}
}

func TestStoryGoalRequiresNoRemainingDeepening(t *testing.T) {
	t.Parallel()
	start := &journal.SessionStart{Session: "global", Cores: []machine.CoreInfo{{Core: 0}, {Core: 1}, {Core: 2}, {Core: 3}}}
	payloads := []journal.Payload{start}
	for i, offset := range []int{-49, -49, -49, -50} {
		payloads = append(payloads, &journal.CorePhase{Core: i, To: journal.PhaseAtLimit, Offset: offset})
	}
	for i, pair := range [][2]int{{0, 1}, {1, 2}, {2, 3}, {3, 0}, {1, 3}} {
		payloads = append(payloads, &journal.Combination{Combination: i + 1, Members: []journal.CombinationMember{{Core: pair[0], Offset: -50}, {Core: pair[1], Offset: -50}}})
	}
	payloads = append(payloads, &journal.ProfileChange{To: []int{-49, -49, -49, -50}},
		&journal.CheckingLap{Lap: 1, Event: journal.LapStart, Steps: config.Default().Checking.Lap},
		&journal.CheckingLap{Lap: 1, Event: journal.LapEnd, Passed: true, Full: true})
	s := Project(dashboardEvents(payloads...))
	if s.checking == nil || s.checking.CleanLaps != 1 || !s.canDeepen {
		t.Fatalf("fixture must retain clean credit with globally deeper profile reachable: %+v", s)
	}
	assertStoryNotGoal(t, s)
	payloads = append(payloads, &journal.Shutdown{Reason: journal.ShutdownSignal})
	assertStoryNotGoal(t, Project(dashboardEvents(payloads...)))
}

func assertStoryNotGoal(t *testing.T, s Snapshot) {
	t.Helper()
	if s.goal() || strings.Contains(storyText(s), "carry into the BIOS") {
		t.Fatalf("clean credit mistaken for finished deepening: %s", storyText(s))
	}
	for _, st := range s.stations() {
		if strings.Join(st.sub, " ") == "no room left" || st.label == "Clean lap" && st.state == reached {
			t.Fatalf("remaining depth shown as exhausted: %+v", st)
		}
	}
}

func TestStoryRerunSeparatesShortRepeatsAndOriginalLength(t *testing.T) {
	t.Parallel()
	for _, originalS := range []int{37, 900} {
		t.Run(fmt.Sprint(originalS), func(t *testing.T) {
			events := dashboardEvents(dashboardSession(), storyConfig(nil, 37),
				&journal.CorePhase{Core: 0, To: journal.PhaseAtLimit, Offset: -20, FailurePoint: new(-21)},
				&journal.CorePhase{Core: 1, To: journal.PhaseAtLimit, Offset: -50},
				&journal.CorePhase{Core: 2, To: journal.PhaseAtLimit, Offset: -50},
				&journal.ProfileChange{To: []int{-20, -50, -50}},
				&journal.TrialIntent{Trial: "failed", Condition: machine.Together, Phase: journal.PhaseChecking, Regime: machine.R1, Workload: "mprime-sse-4k-21k", Core: new(0), Offset: new(-20), Profile: []int{-20, -50, -50}, DurationS: originalS},
				&journal.TrialEnd{Trial: "failed", Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(0)},
				&journal.Failure{Trial: "failed", Signal: machine.ComputationError, Attribution: journal.Attributed, Condition: machine.Together, Regime: machine.R1, Core: new(0), Offset: new(-20), Profile: []int{-20, -50, -50}},
				&journal.TunerDecision{Core: 0, Phase: journal.PhaseChecking, Decision: journal.Backoff, FromOffset: -20, ToOffset: -19, FailurePoint: new(-20)},
				&journal.ProfileChange{From: []int{-20, -50, -50}, To: []int{-19, -50, -50}})
			events[9].Cause = []int{9}
			intent := func(id string, seconds int) *journal.TrialIntent {
				return &journal.TrialIntent{Trial: id, Condition: machine.Together, Phase: journal.PhaseChecking, Regime: machine.R1, Workload: "mprime-sse-4k-21k", Core: new(0), Offset: new(-19), Profile: []int{-19, -50, -50}, DurationS: seconds, Rerun: true}
			}
			short := appendStoryEvents(append([]journal.Event(nil), events...), intent("short", 37), &journal.TrialStart{Trial: "short"})
			s := Project(short)
			for _, text := range []string{storyText(s), strings.Join(s.comingUp(), "\n")} {
				if !strings.Contains(text, "5") || !strings.Contains(text, "starts of 37 s") {
					t.Fatalf("short requirement lost recorded count/duration: %s", text)
				}
				if originalS != 37 && !strings.Contains(text, "one start of 15 min") {
					t.Fatalf("original-length followup omitted: %s", text)
				}
				if originalS == 37 && strings.Contains(text, "one start") {
					t.Fatalf("invented followup when durations coincide: %s", text)
				}
			}
			if originalS == 37 {
				return
			}
			for i := range 5 {
				id := fmt.Sprintf("pass-%d", i)
				events = appendStoryEvents(events, intent(id, 37), &journal.TrialStart{Trial: id}, &journal.TrialEnd{Trial: id, Outcome: journal.OutcomePass, DurationS: 37})
			}
			events = appendStoryEvents(events, intent("long", originalS), &journal.TrialStart{Trial: "long"})
			s = Project(events)
			for _, text := range []string{storyText(s), strings.Join(s.comingUp(), "\n")} {
				if !strings.Contains(text, "15 min") || strings.Contains(text, "5 starts") || strings.Contains(text, "5 passing starts") || strings.Contains(text, "5 times") {
					t.Fatalf("long stage described as another short batch: %s", text)
				}
				if !strings.Contains(text, "once") && !strings.Contains(text, "one original-length start") {
					t.Fatalf("long stage isn't exactly one start: %s", text)
				}
			}
		})
	}
}

func TestStoryInconclusiveResetIsNotRecentCrash(t *testing.T) {
	t.Parallel()
	for _, reason := range []machine.ResetKind{machine.ResetPowerLoss, machine.ResetThermalTrip} {
		s := Project(dashboardEvents(dashboardSession(), &journal.CrashDetected{PreviousBoot: "previous", Inconclusive: true, ResetReason: reason}))
		if text := storyText(s); strings.Contains(text, "machine crashed and rebooted") {
			t.Fatalf("inconclusive %s reset asserted as crash: %s", reason, text)
		}
	}
}

func TestStoryCreditedLapAfterBackoffStillHasDepthToFind(t *testing.T) {
	t.Parallel()
	oldProfile, backedOff := []int{-20, -20, -50}, []int{-20, -19, -50}
	steps := config.Default().Checking.Lap
	w := machine.Workloads(machine.R2)[0].ID
	events := dashboardEvents(dashboardSession(),
		&journal.CorePhase{Core: 0, To: journal.PhaseAtLimit, Offset: -20, Pass: new(-20)},
		&journal.CorePhase{Core: 1, To: journal.PhaseAtLimit, Offset: -20, Pass: new(-20), FailurePoint: new(-21)},
		&journal.CorePhase{Core: 2, To: journal.PhaseAtLimit, Offset: -50, Pass: new(-50)},
		&journal.Combination{Combination: 1, Members: []journal.CombinationMember{{Core: 0, Offset: -21}, {Core: 1, Offset: -20}}},
		&journal.ProfileChange{To: oldProfile},
		&journal.CheckingLap{Lap: 1, Event: journal.LapStart, Steps: steps},
		&journal.CheckingLap{Lap: 1, Event: journal.LapEnd, Passed: true, Full: true},
		&journal.CheckingLap{Lap: 2, Event: journal.LapStart, Steps: steps},
		&journal.TrialIntent{Trial: "fail", Core: new(1), Offset: new(-20), Regime: machine.R2, Workload: w, DurationS: 120, Condition: machine.Together, Phase: journal.PhaseChecking, Lap: 2, Profile: oldProfile},
		&journal.TrialStart{Trial: "fail"},
		&journal.TrialEnd{Trial: "fail", Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(1), DurationS: 10},
		&journal.Failure{Trial: "fail", Signal: machine.ComputationError, Attribution: journal.Attributed, Core: new(1), Offset: new(-20), Regime: machine.R2, Condition: machine.Together, Profile: oldProfile},
		&journal.TunerDecision{Core: 1, Phase: journal.PhaseChecking, Decision: journal.Backoff, FromOffset: -20, ToOffset: -19, FailurePoint: new(-20)},
		&journal.CorePhase{Core: 0, From: journal.PhaseAtLimit, To: journal.PhaseHasRoom, Offset: -20, Pass: new(-20)},
		&journal.ProfileChange{From: oldProfile, To: backedOff})
	events[13].Cause = []int{13}
	s := Project(events)
	if s.checking == nil || s.checking.CleanLaps != 1 || s.core(0).phase != journal.PhaseHasRoom || s.rerunDuration != 120*time.Second {
		t.Fatalf("fixture lost credited lap, freed depth or pending rerun: %+v", s)
	}
	assertStoryNotGoal(t, s)
	for i := range 5 {
		id := fmt.Sprintf("backoff-pass-%d", i)
		events = appendStoryEvents(events,
			&journal.TrialIntent{Trial: id, Core: new(1), Offset: new(-19), Regime: machine.R2, Workload: w, DurationS: 120, Condition: machine.Together, Phase: journal.PhaseChecking, Profile: backedOff, Rerun: true},
			&journal.TrialStart{Trial: id},
			&journal.TrialEnd{Trial: id, Outcome: journal.OutcomePass, DurationS: 120})
	}
	s = Project(events)
	if s.checking.CleanLaps != 1 || !s.canDeepen || s.rerunDuration != 0 {
		t.Fatalf("fixture did not finish reruns with deepening still due: %+v", s)
	}
	assertStoryNotGoal(t, s)
}

func TestStoryGoalRetainsCompletedFullSchedule(t *testing.T) {
	t.Parallel()
	s := Project(dashboardEvents(dashboardSession(),
		&journal.CorePhase{Core: 0, To: journal.PhaseAtLimit, Offset: -50},
		&journal.CorePhase{Core: 1, To: journal.PhaseAtLimit, Offset: -50},
		&journal.CorePhase{Core: 2, To: journal.PhaseAtLimit, Offset: -50},
		&journal.ProfileChange{To: []int{-50, -50, -50}},
		&journal.CheckingLap{Lap: 1, Event: journal.LapStart, Steps: config.Default().Checking.Lap},
		&journal.CheckingLap{Lap: 1, Event: journal.LapEnd, Passed: true, Full: true},
		&journal.TrialIntent{Trial: "watch", Core: new(0), Offset: new(-50), Regime: machine.R1, Workload: machine.Workloads(machine.R1)[0].ID, DurationS: 120, Condition: machine.Together, Phase: journal.PhaseChecking, Profile: []int{-50, -50, -50}},
		&journal.TrialStart{Trial: "watch"}))
	if !s.goal() || !strings.Contains(storyText(s), "carry into the BIOS") {
		t.Fatalf("completed full schedule lost its normal goal narration: %s", storyText(s))
	}
	for _, st := range s.stations() {
		if st.label == "Go deeper" && strings.Join(st.sub, " ") != "no room left" || st.label == "Clean lap" && st.state != reached {
			t.Fatalf("completed goal station changed: %+v", st)
		}
	}
}

func TestStoryQueuedResetIsNotCompletedGoal(t *testing.T) {
	t.Parallel()
	events := dashboardEvents(dashboardSession(),
		&journal.CorePhase{Core: 0, To: journal.PhaseAtLimit, Offset: -50},
		&journal.CorePhase{Core: 1, To: journal.PhaseAtLimit, Offset: -50},
		&journal.CorePhase{Core: 2, To: journal.PhaseAtLimit, Offset: -50},
		&journal.ProfileChange{To: []int{-50, -50, -50}},
		&journal.CheckingLap{Lap: 1, Event: journal.LapStart, Steps: config.Default().Checking.Lap},
		&journal.CheckingLap{Lap: 1, Event: journal.LapEnd, Passed: true, Full: true})
	if !Project(events).goal() {
		t.Fatal("fixture never reached the goal before reset")
	}
	events = appendStoryEvents(events, &journal.CommandReset{Core: new(0)})
	s := Project(events)
	if s.core(0).phase != journal.PhaseAtLimit || !s.core(0).queued {
		t.Fatalf("fixture must retain old phase while reset is queued: %+v", s.core(0))
	}
	assertStoryNotGoal(t, s)
}

func TestStoryGoalSurvivesPartialScheduleReload(t *testing.T) {
	t.Parallel()
	for _, stopped := range []bool{false, true} {
		t.Run(fmt.Sprint(stopped), func(t *testing.T) {
			t.Parallel()
			events := dashboardEvents(dashboardSession(),
				&journal.CorePhase{Core: 0, To: journal.PhaseAtLimit, Offset: -50},
				&journal.CorePhase{Core: 1, To: journal.PhaseAtLimit, Offset: -50},
				&journal.CorePhase{Core: 2, To: journal.PhaseAtLimit, Offset: -50},
				&journal.ProfileChange{To: []int{-50, -50, -50}},
				&journal.CheckingLap{Lap: 1, Event: journal.LapStart, Steps: config.Default().Checking.Lap},
				&journal.CheckingLap{Lap: 1, Event: journal.LapEnd, Passed: true, Full: true},
				storyConfig([]machine.Regime{machine.R1}, 120))
			if stopped {
				events = appendStoryEvents(events, &journal.Shutdown{Reason: journal.ShutdownLaps, Laps: 1})
			}
			s := Project(events)
			if s.checking == nil || s.checking.CleanLaps != 1 || s.checking.Full || s.canDeepen || s.rerunDuration != 0 {
				t.Fatalf("fixture must retain full credit while only the next schedule is partial: %+v", s)
			}
			if !s.goal() {
				t.Fatalf("next schedule erased an already completed goal: %s", storyText(s))
			}
			if stopped && !strings.Contains(storyText(s), "carry into the BIOS") || !stopped && s.headline() != "KEEPING WATCH" {
				t.Fatalf("completed-goal narration was lost after reload: %+v", s.story(time.Unix(1100, 0).UTC()))
			}
			for _, st := range s.stations() {
				if st.label == "Clean lap" && st.state != reached {
					t.Fatalf("completed goal was not retained: %+v", st)
				}
			}
			next := strings.Join(s.lapNext(), "\n")
			if !strings.Contains(next, "future laps don't add clean-lap credit") || !strings.Contains(next, "goal is already reached") {
				t.Fatalf("partial future schedule confused prior credit: %s", next)
			}
		})
	}
}

func TestStoryShutdownClaimsRestorationOnlyForRunStops(t *testing.T) {
	t.Parallel()
	for _, reason := range []journal.ShutdownReason{journal.ShutdownCommand, journal.ShutdownSignal, journal.ShutdownLaps, journal.ShutdownDeadEnd} {
		t.Run(string(reason), func(t *testing.T) {
			s := Project(dashboardEvents(dashboardSession(),
				&journal.ProfileApplied{Offsets: []int{-20, -30, -10}, Condition: machine.Together},
				&journal.Shutdown{Reason: reason}))
			text := storyText(s)
			if s.stoppedReason != reason {
				t.Fatalf("lost shutdown reason: got %q, want %q", s.stoppedReason, reason)
			}
			restored := strings.Contains(text, "put the offsets back to safe values")
			if restored != (reason == journal.ShutdownSignal || reason == journal.ShutdownLaps) {
				t.Fatalf("shutdown %q misstates restoration: %s", reason, text)
			}
			if reason == journal.ShutdownCommand && (!strings.Contains(text, "without changing the applied offsets") || strings.Contains(text, "not what is applied now")) {
				t.Fatalf("command shutdown implies hardware changed: %s", text)
			}
		})
	}
}

func TestStoryFullHuntPassRestartsBinaryGroups(t *testing.T) {
	t.Parallel()
	events := dashboardEvents(dashboardSession(),
		&journal.HuntStart{Hunt: 1, Regime: machine.R7, Workload: "mprime-avx2-36k-248k-allcore", Cores: []int{0, 1, 2}, Parked: []int{0, 0, 0}, Failing: []int{-20, -30, -10}, Candidates: []int{0, 1, 2}, Starts: 5, StartS: 120, DurationS: 600},
		&journal.HuntGroup{Hunt: 1, Group: 3, Stage: "full", Cores: []int{0, 1, 2}, Profile: []int{-20, -30, -10}, DurationS: 120},
		&journal.TrialIntent{Trial: "full", Condition: machine.Parked, Phase: journal.PhaseHunt, Regime: machine.R7, Workload: "mprime-avx2-36k-248k-allcore", Cores: []int{0, 1, 2}, Profile: []int{-20, -30, -10}, DurationS: 120, Hunt: 1, Group: 3},
		&journal.TrialStart{Trial: "full"})
	text := storyText(Project(events))
	if !strings.Contains(text, "split the original candidates in two") || !strings.Contains(text, "restart group trials at the length of the original trial") || strings.Contains(text, "I repeat it") {
		t.Fatalf("full-group pass promises the wrong next trial: %s", text)
	}
}

func TestStoryCheckingGoalIncludesRecordOnlyCompletion(t *testing.T) {
	t.Parallel()
	s := Snapshot{checkingFull: true}
	st := s.lapStory(&trial{regime: machine.R7, cores: []int{0}, recordOnly: true})
	text := strings.Join(st.paragraphs, "\n")
	for _, want := range []string{"ordinary steps must pass", "record-only partial steps only need to complete", "The lap goes on either way"} {
		if !strings.Contains(text, want) {
			t.Fatalf("checking goal contradicts record-only completion: missing %q in %s", want, text)
		}
	}
}
