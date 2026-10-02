package watch

import (
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/charmbracelet/x/ansi"
	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestTileMarksAndMask(t *testing.T) {
	tile := tile{phase: journal.PhaseDone, number: -20, hasNumber: true, fail: new(-40), joint: []int{-30}, trying: new(-25), anchor: new(-35)}
	for _, tc := range []struct {
		depth int
		want  string
	}{
		{20, "█"}, {25, ">"}, {30, "j"}, {35, "·"}, {40, "x"}, {45, "."},
	} {
		_, got := tile.cellLook(tile.kindAt(tc.depth, true))
		if diff := cmp.Diff(tc.want, got); diff != "" {
			t.Errorf("depth %d glyph (-want +got):\n%s", tc.depth, diff)
		}
	}
	if got := tile.gauge(50); strings.ContainsAny(got, "▒:") {
		t.Errorf("obsolete glyph in gauge %q", got)
	}
}

func TestActivitySummary(t *testing.T) {
	s := Snapshot{huntID: 3, maskID: 4, round: 2, tiles: []tile{{phase: journal.PhaseDone}, {phase: journal.PhaseResident}}}
	want := []string{"hunt 3 mask 4", "refine round 2", "1/2 done", "0 failures", "0 crashes"}
	if diff := cmp.Diff(want, s.summary()); diff != "" {
		t.Errorf("summary (-want +got):\n%s", diff)
	}
}

func TestGuardQualificationOutsideGuardPhase(t *testing.T) {
	gs := &journal.GuardState{Rotation: 2, Steps: []machine.Regime{machine.R1}, StepsDone: 1, Missing: []string{"R2 needs 3 steps, has 0"}, CleanRotations: 1, LastQualifiedRotation: 1}
	s := Snapshot{huntID: 3, guardState: gs, tiles: []tile{{phase: journal.PhaseDone}}}
	want := []string{"hunt 3", "1/1 done", "qualified rotations 1 since last deepening, latest 1", "0 failures", "0 crashes"}
	if diff := cmp.Diff(want, s.summary()); diff != "" {
		t.Errorf("summary during a hunt (-want +got):\n%s", diff)
	}
	s.guard = true
	if got := s.scheduleLine(); !strings.Contains(got, "not qualifying: R2 needs 3 steps, has 0") {
		t.Errorf("schedule of a non-qualifying rotation %q lacks the missing coverage", got)
	}
}

func TestSessionWarningVisibleInDashboard(t *testing.T) {
	now := time.Date(2026, 9, 30, 12, 0, 0, 0, time.UTC)
	warning := "trial 0001 passed, but retain passed trial failed: permission denied"
	events := []journal.Event{
		{Seq: 1, Time: now, Kind: journal.KindSessionStart, Data: &journal.SessionStart{Session: "warning", Cores: []machine.CoreInfo{{Core: 0, CPUs: []int{0, 1}}}}},
		{Seq: 2, Time: now, Kind: journal.KindSessionWarning, Msg: warning, Data: &journal.SessionWarning{Operation: "retain passed trial", Trial: "0001", Error: "permission denied"}},
	}
	frame := Render(Project(events), 160, 40, now)
	if !strings.Contains(frame, warning) {
		t.Fatalf("session maintenance warning absent from dashboard:\n%s", frame)
	}
}

func TestDashboardActivityFrames(t *testing.T) {
	t.Parallel()
	now := time.Unix(1100, 0).UTC()
	for _, tc := range []struct {
		name          string
		events        []journal.Event
		width, height int
	}{
		{"search", dashboardEvents(dashboardSession(),
			&journal.CorePhase{Core: 0, To: journal.PhaseSearch, Offset: -25, Pass: new(-20), FailedMark: new(-40)},
			&journal.TrialIntent{Trial: "search", Core: new(0), Offset: new(-25), Condition: machine.Isolated, Phase: journal.PhaseSearch, Regime: machine.R1, Workload: "mprime-sse-4k-21k", DurationS: 90, Profile: []int{-25, 0, 0}},
			&journal.TrialStart{Trial: "search"}), 161, 60},
		{"hunt", dashboardHuntEvents(), 50, 55},
		{"refine", dashboardRefineEvents(), 161, 60},
		{"dead-end", dashboardEvents(dashboardSession(),
			&journal.Failure{Signal: machine.ComputationError, Attribution: journal.Attributed, Core: new(0), Offset: new(0), Condition: machine.Isolated, Profile: []int{0, 0, 0}},
			&journal.CorePhase{Core: 0, To: journal.PhaseSearch, FailedMark: new(0)},
			&journal.DeadEnd{Condition: journal.DeadEndFailureAtZero, Core: new(0), Detail: "core 00 failed at CO 0"}), 60, 24},
		{"guard", dashboardEvents(dashboardSession(),
			&journal.CorePhase{Core: 0, To: journal.PhaseResident, Offset: -20},
			&journal.CorePhase{Core: 1, To: journal.PhaseDone, Offset: -30, FailedMark: new(-31)},
			&journal.CorePhase{Core: 2, To: journal.PhaseResident, Offset: -10},
			&journal.ProfileChange{To: []int{-20, -30, -10}},
			&journal.GuardRotation{Rotation: 1, Event: journal.RotationStart, Steps: []machine.Regime{machine.R1, machine.R2, machine.R7}}), 161, 60},
	} {
		t.Run(tc.name, func(t *testing.T) {
			golden(t, "activity-"+tc.name, ansi.Strip(Render(Project(tc.events), tc.width, tc.height, now))+"\n")
		})
	}
}

func TestTrialProgressBoundaries(t *testing.T) {
	t.Parallel()
	now := time.Unix(1100, 0).UTC()
	for _, tc := range []struct {
		name       string
		started    time.Time
		duration   time.Duration
		hasStarted bool
		elapsed    int
	}{
		{"intent only", now.Add(-time.Minute), 90 * time.Second, false, 0},
		{"future start", now.Add(time.Second), 90 * time.Second, true, 0},
		{"past deadline", now.Add(-100 * time.Second), 90 * time.Second, true, 90},
	} {
		t.Run(tc.name, func(t *testing.T) {
			s := Snapshot{trial: &trial{cores: []int{0}, condition: machine.Isolated, regime: machine.R1, workload: "test", started: tc.started, hasStarted: tc.hasStarted, duration: tc.duration}}
			n := 0
			if tc.duration > 0 {
				n = 30 * tc.elapsed / int(tc.duration.Seconds())
			}
			want := fmt.Sprintf("core 00   isolated   R1 test   %s%s %d/%ds", strings.Repeat("█", n), strings.Repeat("░", 30-n), tc.elapsed, int(tc.duration.Seconds()))
			if diff := cmp.Diff(want, ansi.Strip(s.trialLine(now))); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestDashboardThermalSummary(t *testing.T) {
	t.Parallel()
	s := Snapshot{
		tiles:      []tile{{phase: journal.PhaseDone}, {phase: journal.PhaseResident}},
		failures:   1,
		crashes:    1,
		tctlTrial:  new(81),
		guardState: &journal.GuardState{CleanRotations: 2, LastQualifiedRotation: 3, TctlMaxC: new(87)},
	}
	want := []string{"1/2 done", "qualified rotations 2 since last deepening, latest 3", "Tctl profile max 87 C", "1 failure", "1 crash", "Tctl last trial max 81 C"}
	if diff := cmp.Diff(want, s.summary()); diff != "" {
		t.Fatal(diff)
	}
	s.guard = true
	if diff := cmp.Diff(want[:len(want)-1], s.summary()); diff != "" {
		t.Fatal(diff)
	}
}

func TestGuardRotationProgress(t *testing.T) {
	t.Parallel()
	s := Snapshot{guard: true, guardState: &journal.GuardState{
		Rotation: 4, RotationOpen: true, Steps: []machine.Regime{machine.R1, machine.R2, machine.R7}, StepsDone: 1,
	}}
	if diff := cmp.Diff("rotation 4   step 2/3   R1 | R2 | R7", ansi.Strip(s.scheduleLine())); diff != "" {
		t.Fatal(diff)
	}
}

func TestNextSearchCores(t *testing.T) {
	t.Parallel()
	s := Snapshot{order: []int{0, 2, 1, 3, 4, 5}, current: 1, tiles: []tile{
		{id: 0, phase: journal.PhaseSearch},
		{id: 1, phase: journal.PhaseSearch},
		{id: 2, phase: journal.PhaseResident},
		{id: 3, phase: journal.PhaseSearch},
		{id: 4, phase: journal.PhaseSearch},
		{id: 5, phase: journal.PhaseSearch},
	}}
	if diff := cmp.Diff("turn order 00 02 01 03 04 05   up next 03, 04, 05", ansi.Strip(s.turnOrder())); diff != "" {
		t.Fatal(diff)
	}
	s.current = 5
	if diff := cmp.Diff("turn order 00 02 01 03 04 05   up next 00, 01, 03", ansi.Strip(s.turnOrder())); diff != "" {
		t.Fatal(diff)
	}
}

func TestTrialTargets(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		cores []int
		all   bool
		want  string
	}{
		{[]int{0}, false, "core 00"},
		{[]int{0, 2}, false, "cores 00 02"},
		{[]int{0, 1, 2}, true, "all 3 cores"},
	} {
		s := Snapshot{trial: &trial{cores: tc.cores, all: tc.all, condition: machine.Resident, regime: machine.R7, workload: "AVX2", duration: 120 * time.Second}}
		if diff := cmp.Diff(tc.want, strings.Split(ansi.Strip(s.trialLine(time.Unix(1000, 0))), "   ")[0]); diff != "" {
			t.Fatal(diff)
		}
	}
	s := Snapshot{inFlight: "SMU set all cores to CO 0"}
	if diff := cmp.Diff(s.inFlight, ansi.Strip(s.trialLine(time.Unix(1000, 0)))); diff != "" {
		t.Fatal(diff)
	}
}
