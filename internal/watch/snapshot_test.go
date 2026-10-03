package watch

import (
	"fmt"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func dashboardEvents(payloads ...journal.Payload) []journal.Event {
	var events []journal.Event
	at := time.Unix(1000, 0).UTC()
	for i, p := range payloads {
		events = append(events, journal.Event{Seq: i + 1, Time: at.Add(time.Duration(i) * time.Second), Boot: "boot", Kind: p.Kind(), Msg: p.Message(), Data: p})
	}
	return events
}

func dashboardSession() *journal.SessionStart {
	return &journal.SessionStart{Session: "dashboard", Cores: []machine.CoreInfo{{Core: 0, CCD: 0, CPUs: []int{0, 1}}, {Core: 1, CCD: 0, CPUs: []int{2, 3}}, {Core: 2, CCD: 1, CPUs: []int{4, 5}}}}
}

func dashboardHuntEvents() []journal.Event {
	return dashboardEvents(dashboardSession(),
		&journal.CorePhase{Core: 0, To: journal.PhaseResident, Offset: -20},
		&journal.CorePhase{Core: 1, To: journal.PhaseDone, Offset: -30, FailedMark: new(-31)},
		&journal.CorePhase{Core: 2, To: journal.PhaseResident, Offset: -10},
		&journal.MarkJoint{Mark: 1, Members: []journal.JointMember{{Core: 0, Offset: -25}, {Core: 2, Offset: -15}}},
		&journal.TrialIntent{Trial: "previous", Condition: machine.Resident, Phase: journal.PhaseGuard, Regime: machine.R7, Workload: "mprime-avx2-36k-248k-allcore", Cores: []int{0, 1, 2}, Profile: []int{-20, -30, -10}, DurationS: 120},
		&journal.TrialEnd{Trial: "previous", Outcome: journal.OutcomeFailure, Signal: machine.Crash},
		&journal.Failure{Trial: "previous", Signal: machine.Crash, Attribution: journal.Unattributed, Condition: machine.Resident, Regime: machine.R7, Profile: []int{-20, -30, -10}},
		&journal.HuntStart{Hunt: 3, Regime: machine.R7, Workload: "mprime-avx2-36k-248k-allcore", Cores: []int{0, 1, 2}, Anchor: []int{-10, -30, -5}, Failing: []int{-20, -30, -10}, Candidates: []int{0, 2}, Starts: 5, StartS: 120, DurationS: 120},
		&journal.HuntMask{Hunt: 3, Mask: 4, Cores: []int{2}, Profile: []int{-10, -30, -10}, DurationS: 120},
		&journal.TrialIntent{Trial: "mask", Condition: machine.Masked, Phase: journal.PhaseHunt, Regime: machine.R7, Workload: "mprime-avx2-36k-248k-allcore", Cores: []int{0, 1, 2}, Profile: []int{-10, -30, -10}, DurationS: 120, Hunt: 3, Mask: 4})
}

func dashboardRefineEvents() []journal.Event {
	return dashboardEvents(dashboardSession(),
		&journal.CorePhase{Core: 0, To: journal.PhaseResident, Offset: -20},
		&journal.CorePhase{Core: 1, To: journal.PhaseDone, Offset: -30, FailedMark: new(-31)},
		&journal.CorePhase{Core: 2, To: journal.PhaseResident, Offset: -10},
		&journal.RefineRound{Round: 2, Event: journal.RotationStart, Anchor: []int{-20, -30, -10}, Target: []int{-22, -30, -10}, Profile: []int{-21, -30, -10}, Cores: []int{0}, Starts: 5, StartS: 120})
}

func TestProjectTrialLifecycle(t *testing.T) {
	t.Parallel()
	intent := &journal.TrialIntent{Trial: "one", Core: new(0), Offset: new(-25), Condition: machine.Isolated, Phase: journal.PhaseSearch, Regime: machine.R1, Workload: "mprime-sse-4k-21k", DurationS: 90, Profile: []int{-25, 0, 0}}
	events := dashboardEvents(dashboardSession(), &journal.CorePhase{Core: 0, To: journal.PhaseSearch, Offset: -25, Pass: new(-20), FailedMark: new(-30)}, intent)
	s := Project(events)
	wantTile := tile{id: 0, phase: journal.PhaseSearch, number: -20, hasNumber: true, fail: new(-30), trying: new(-25), loaded: true}
	if diff := cmp.Diff(wantTile, s.tiles[0], cmp.AllowUnexported(tile{})); diff != "" {
		t.Fatal(diff)
	}
	wantTrial := &trial{cores: []int{0}, condition: machine.Isolated, regime: machine.R1, workload: "mprime SSE 4K-21K", duration: 90 * time.Second}
	if diff := cmp.Diff(wantTrial, s.trial, cmp.AllowUnexported(trial{})); diff != "" {
		t.Fatal(diff)
	}
	if diff := cmp.Diff([]int{0, 2, 1}, s.order); diff != "" {
		t.Fatal(diff)
	}
	if s.current != 0 {
		t.Fatalf("current core = %d", s.current)
	}
	events = append(events, journal.Event{Seq: 4, Time: events[2].Time.Add(time.Second), Boot: "boot", Kind: journal.KindTrialStart, Data: &journal.TrialStart{Trial: "one"}})
	s = Project(events)
	if !s.trial.hasStarted || !s.trial.started.Equal(events[3].Time) {
		t.Fatalf("trial start not projected: %+v", s.trial)
	}
	end := journal.Event{Seq: 5, Time: events[3].Time.Add(90 * time.Second), Boot: "boot", Kind: journal.KindTrialEnd, Data: &journal.TrialEnd{Trial: "one", Outcome: journal.OutcomePass, DurationS: 90, TctlMaxC: new(81)}}
	events = append(events, end)
	s = Project(events)
	if s.trial != nil || s.tctlTrial == nil || *s.tctlTrial != 81 {
		t.Fatalf("completed trial projection: %+v", s)
	}
}

func TestProjectTrialStartSelection(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name       string
		payloads   []journal.Payload
		startIndex int
		zeroTime   bool
	}{
		{
			name: "before intent",
			payloads: []journal.Payload{
				&journal.TrialStart{Trial: "one"},
				&journal.TrialIntent{Trial: "one"},
			},
			startIndex: 1,
		},
		{
			name: "latest matching start",
			payloads: []journal.Payload{
				&journal.TrialIntent{Trial: "one"},
				&journal.TrialStart{Trial: "one"},
				&journal.TrialStart{Trial: "one"},
				&journal.TrialStart{Trial: "other"},
			},
			startIndex: 3,
		},
		{
			name: "only unrelated start",
			payloads: []journal.Payload{
				&journal.TrialIntent{Trial: "one"},
				&journal.TrialStart{Trial: "other"},
			},
			startIndex: -1,
		},
		{
			name: "zero timestamp",
			payloads: []journal.Payload{
				&journal.TrialIntent{Trial: "one"},
				&journal.TrialStart{Trial: "one"},
			},
			startIndex: 2,
			zeroTime:   true,
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			events := dashboardEvents(append([]journal.Payload{dashboardSession()}, tc.payloads...)...)
			var started time.Time
			if tc.startIndex >= 0 {
				if tc.zeroTime {
					events[tc.startIndex].Time = time.Time{}
				}
				started = events[tc.startIndex].Time
			}
			s := Project(events)
			if s.trial == nil {
				t.Fatal("open trial not projected")
			}
			if diff := cmp.Diff(tc.startIndex >= 0, s.trial.hasStarted); diff != "" {
				t.Fatalf("trial start presence (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff(started, s.trial.started); diff != "" {
				t.Fatalf("trial start time (-want +got):\n%s", diff)
			}
		})
	}
}

func TestProjectMaskedHunt(t *testing.T) {
	t.Parallel()
	events := dashboardHuntEvents()
	s := Project(events)
	want := []tile{
		{id: 0, phase: journal.PhaseResident, number: -20, hasNumber: true, joint: []int{-25}, hunt: true, masked: true, anchor: new(-10), loaded: true},
		{id: 1, phase: journal.PhaseDone, number: -30, hasNumber: true, fail: new(-31), anchor: new(-30), loaded: true},
		{id: 2, ccd: 1, phase: journal.PhaseResident, number: -10, hasNumber: true, joint: []int{-15}, hunt: true, masked: true, trying: new(-10), loaded: true},
	}
	if diff := cmp.Diff(want, s.tiles, cmp.AllowUnexported(tile{})); diff != "" {
		t.Fatal(diff)
	}
	if s.huntID != 3 || s.maskID != 4 || s.trial.workload != "mprime AVX2 36K-248K all-core" {
		t.Fatalf("hunt identity or unknown workload lost: %+v", s)
	}
}

func TestProjectActivityAndRecentBound(t *testing.T) {
	t.Parallel()
	if diff := cmp.Diff(Snapshot{}, Project(nil), cmp.AllowUnexported(Snapshot{})); diff != "" {
		t.Fatal(diff)
	}
	events := dashboardEvents(dashboardSession(),
		&journal.Failure{Signal: machine.Crash, Attribution: journal.Attributed, Core: new(1), Offset: new(0), Condition: machine.Isolated, Profile: []int{0, 0, 0}},
		&journal.CrashDetected{PreviousBoot: "old"},
		&journal.CorePhase{Core: 1, To: journal.PhaseSearch, FailedMark: new(0)},
		&journal.DeadEnd{Condition: journal.DeadEndFailureAtZero, Core: new(1), Detail: "core 01 failed at CO 0"})
	s := Project(events)
	if s.failures != 1 || s.crashes != 1 || s.lastFailure == nil || s.lastFailure.msg != events[1].Msg || s.deadEnd != "dead end failure_at_zero: core 01 failed at CO 0" {
		t.Fatalf("activity projection: %+v", s)
	}
	for i := range recentLimit + 5 {
		e := dashboardEvents(&journal.SessionWarning{Operation: "write state projection", Error: fmt.Sprint(i)})[0]
		e.Seq = len(events) + 1
		e.Msg = fmt.Sprintf("warning %d", i)
		events = append(events, e)
	}
	s = Project(events)
	if len(s.recent) != recentLimit || s.recent[0].msg != "warning 5" || s.recent[recentLimit-1].msg != "warning 104" {
		t.Fatalf("recent history not bounded to newest events: %+v", s.recent)
	}
}

func TestProjectResidentTrial(t *testing.T) {
	t.Parallel()
	events := dashboardEvents(dashboardSession(),
		&journal.CorePhase{Core: 0, To: journal.PhaseResident, Offset: -20},
		&journal.CorePhase{Core: 1, To: journal.PhaseResident, Offset: -30},
		&journal.CorePhase{Core: 2, To: journal.PhaseResident, Offset: -10},
		&journal.TrialIntent{Trial: "resident", Condition: machine.Resident, Phase: journal.PhaseGuard, Regime: machine.R7, Workload: "future-workload", Cores: []int{0, 1, 2}, Profile: []int{-20, -30, -10}, DurationS: 120})
	s := Project(events)
	if s.trial == nil || !s.trial.all || s.trial.workload != "future-workload" {
		t.Fatalf("resident target: %+v", s.trial)
	}
	for _, tile := range s.tiles {
		if !tile.loaded || tile.trying != nil || tile.masked || tile.anchor != nil {
			t.Fatalf("resident trial acquired isolated or masked decorations: %+v", tile)
		}
	}
}

func TestProjectRefinementRound(t *testing.T) {
	t.Parallel()
	s := Project(dashboardRefineEvents())
	if diff := cmp.Diff(2, s.round); diff != "" {
		t.Fatal(diff)
	}
	if s.huntID != 0 || s.guard {
		t.Fatalf("refinement presented as hunt or guard: %+v", s)
	}
}

func TestProjectInFlightAction(t *testing.T) {
	t.Parallel()
	s := Project(dashboardEvents(dashboardSession(), &journal.SMUIntent{Op: journal.SMUSetAll, Offset: 0}))
	if diff := cmp.Diff("SMU set all cores to CO 0", s.inFlight); diff != "" {
		t.Fatal(diff)
	}
	if s.trial != nil {
		t.Fatalf("SMU intent presented as a trial: %+v", s.trial)
	}
}
