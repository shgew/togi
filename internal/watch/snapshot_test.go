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
		&journal.CorePhase{Core: 0, To: journal.PhaseHasRoom, Offset: -20},
		&journal.CorePhase{Core: 1, To: journal.PhaseAtLimit, Offset: -30, FailurePoint: new(-31)},
		&journal.CorePhase{Core: 2, To: journal.PhaseHasRoom, Offset: -10},
		&journal.Combination{Combination: 1, Members: []journal.CombinationMember{{Core: 0, Offset: -25}, {Core: 2, Offset: -15}}},
		&journal.TrialIntent{Trial: "previous", Condition: machine.Together, Phase: journal.PhaseChecking, Regime: machine.R7, Workload: "mprime-avx2-36k-248k-allcore", Cores: []int{0, 1, 2}, Profile: []int{-20, -30, -10}, DurationS: 120},
		&journal.TrialEnd{Trial: "previous", Outcome: journal.OutcomeFailure, Signal: machine.Crash},
		&journal.Failure{Trial: "previous", Signal: machine.Crash, Attribution: journal.Unattributed, Condition: machine.Together, Regime: machine.R7, Profile: []int{-20, -30, -10}},
		&journal.HuntStart{Hunt: 3, Regime: machine.R7, Workload: "mprime-avx2-36k-248k-allcore", Cores: []int{0, 1, 2}, Parked: []int{-10, -30, -5}, Failing: []int{-20, -30, -10}, Candidates: []int{0, 2}, Starts: 5, StartS: 120, DurationS: 120},
		&journal.HuntGroup{Hunt: 3, Group: 4, Cores: []int{2}, Profile: []int{-10, -30, -10}, DurationS: 120},
		&journal.TrialIntent{Trial: "group", Condition: machine.Parked, Phase: journal.PhaseHunt, Regime: machine.R7, Workload: "mprime-avx2-36k-248k-allcore", Cores: []int{0, 1, 2}, Profile: []int{-10, -30, -10}, DurationS: 120, Hunt: 3, Group: 4},
		&journal.TrialStart{Trial: "group"})
}

func dashboardDeepeningEvents() []journal.Event {
	return dashboardEvents(dashboardSession(),
		&journal.CorePhase{Core: 0, To: journal.PhaseHasRoom, Offset: -20},
		&journal.CorePhase{Core: 1, To: journal.PhaseAtLimit, Offset: -30, FailurePoint: new(-31)},
		&journal.CorePhase{Core: 2, To: journal.PhaseHasRoom, Offset: -10},
		&journal.DeepeningRound{Round: 2, Event: journal.LapStart, Base: []int{-20, -30, -10}, Target: []int{-22, -30, -10}, Profile: []int{-21, -30, -10}, Cores: []int{0}, Starts: 5, StartS: 120})
}

func TestProjectTrialLifecycle(t *testing.T) {
	t.Parallel()
	intent := &journal.TrialIntent{Trial: "one", Core: new(0), Offset: new(-25), Condition: machine.Alone, Phase: journal.PhaseSearch, Regime: machine.R1, Workload: "mprime-sse-4k-21k", DurationS: 90, Profile: []int{-25, 0, 0}}
	events := dashboardEvents(dashboardSession(),
		&journal.CorePhase{Core: 0, To: journal.PhaseSearch, Offset: -25, Pass: new(-20), FailurePoint: new(-30)},
		&journal.CorePhase{Core: 1, To: journal.PhaseSearch, Offset: -5},
		&journal.CorePhase{Core: 2, To: journal.PhaseSearch, Offset: -5},
		intent,
		&journal.SMUIntent{Op: journal.SMUSet, Core: new(0), Offset: -25},
		&journal.SMUReadback{Core: 0, Offset: -25},
		&journal.TrialStart{Trial: "one"})
	s := Project(events)
	wantTrial := &trial{cores: []int{0}, condition: machine.Alone, regime: machine.R1, workload: "mprime SSE 4K-21K", offset: new(-25), started: events[7].Time, hasStarted: true, duration: 90 * time.Second}
	if diff := cmp.Diff(wantTrial, s.trial, cmp.AllowUnexported(trial{})); diff != "" {
		t.Fatalf("an SMU write after the intent hid the running trial (-want +got):\n%s", diff)
	}
	if c := s.cores[0]; !c.loaded || c.applied != -25 || c.tuned != -25 {
		t.Fatalf("target core not lit at its applied offset: %+v", c)
	}
	if c := s.cores[1]; c.loaded || c.parked {
		t.Fatalf("a core waiting its turn presented as active or parked: %+v", c)
	}
	events = append(events, journal.Event{Seq: 9, Time: events[7].Time.Add(90 * time.Second), Boot: "boot", Kind: journal.KindTrialEnd, Data: &journal.TrialEnd{Trial: "one", Outcome: journal.OutcomePass, DurationS: 90}})
	s = Project(events)
	if s.trial != nil || s.cores[0].loaded {
		t.Fatalf("ended trial still running: %+v", s.trial)
	}
	for _, h := range s.history {
		if h.tag == tagPass {
			t.Fatalf("a search pass crowds the account of what happened: %q", h.sentence())
		}
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

func TestProjectParkedHunt(t *testing.T) {
	t.Parallel()
	s := Project(dashboardHuntEvents())
	got := make([]coreView, len(s.cores))
	for i, c := range s.cores {
		got[i] = coreView{id: c.id, loaded: c.loaded, suspect: c.suspect, parked: c.parked}
	}
	want := []coreView{
		{id: 0, loaded: true, parked: true},
		{id: 1, loaded: true},
		{id: 2, loaded: true, suspect: true},
	}
	if diff := cmp.Diff(want, got, cmp.AllowUnexported(coreView{})); diff != "" {
		t.Fatalf("hunt roles: a candidate outside the group is parked, a group member is a suspect (-want +got):\n%s", diff)
	}
	if s.hunt == nil || s.hunt.id != 3 || s.hunt.group == nil || s.hunt.group.id != 4 || s.trial.workload != "mprime AVX2 36K-248K" {
		t.Fatalf("hunt identity lost: %+v", s.hunt)
	}
	var lines []string
	for _, h := range s.history {
		lines = append(lines, h.tag+": "+h.sentence())
	}
	if diff := cmp.Diff([]string{
		"start: session started on 3 cores",
		"combo: core 00 at -25 and core 02 at -15 fail together",
		"crash: all-core load on all 3 cores, rebooted",
		"hunt: #3 started: which cores caused it?",
	}, lines); diff != "" {
		t.Fatalf("what happened (-want +got):\n%s", diff)
	}
}

func TestFoldEntryFoldsRepeatedPasses(t *testing.T) {
	t.Parallel()
	at := time.Unix(0, 0).UTC()
	pass := func(text string, minutes, peak int) entry {
		at = at.Add(time.Duration(minutes) * time.Minute)
		return entry{at: at, tag: tagPass, text: text, runs: 1, each: time.Duration(minutes) * time.Minute, peak: new(peak)}
	}
	var history []entry
	for _, e := range []entry{
		pass("all-core load on cores 00-07", 2, 70),
		pass("all-core load on cores 00-07", 2, 76),
		pass("all-core load on cores 00-07", 2, 62),
		pass("all-core load on cores 00-07", 10, 69),
		{at: time.Unix(16*60, 0).UTC(), tag: tagCrash, text: "all-core load on cores 00-07, rebooted"},
		pass("all-core load on cores 00-07", 10, 60),
	} {
		history = foldEntry(history, e)
	}
	var got []string
	for _, e := range history {
		got = append(got, e.at.Format("15:04")+" "+e.tag+": "+e.sentence())
	}
	if diff := cmp.Diff([]string{
		"00:06 pass: all-core load on cores 00-07, 3 runs of 2 min, peak 76 C",
		"00:16 pass: all-core load on cores 00-07, 10 min, peak 69 C",
		"00:16 crash: all-core load on cores 00-07, rebooted",
		"00:26 pass: all-core load on cores 00-07, 10 min, peak 60 C",
	}, got); diff != "" {
		t.Fatalf("passes of one test fold into one line with the latest time and the hottest peak, and nothing else folds (-want +got):\n%s", diff)
	}
}

func TestProjectBoundsHistory(t *testing.T) {
	t.Parallel()
	if diff := cmp.Diff(Snapshot{}, Project(nil), cmp.AllowUnexported(Snapshot{})); diff != "" {
		t.Fatal(diff)
	}
	events := dashboardEvents(dashboardSession())
	for i := range 2*historyLimit + 5 {
		e := dashboardEvents(&journal.SessionWarning{Operation: "write state projection", Error: fmt.Sprint(i)})[0]
		e.Seq = len(events) + 1
		e.Msg = fmt.Sprintf("warning %d", i)
		events = append(events, e)
	}
	s := Project(events)
	if len(s.history) != historyLimit || s.history[historyLimit-1].text != fmt.Sprintf("warning %d", 2*historyLimit+4) {
		t.Fatalf("history not bounded to the newest events: %d entries, last %+v", len(s.history), s.history[len(s.history)-1])
	}
}

func TestProjectStoppedShowsTunedOffsets(t *testing.T) {
	t.Parallel()
	s := Project(dashboardEvents(dashboardSession(),
		&journal.CorePhase{Core: 0, To: journal.PhaseAtLimit, Offset: -20, FailurePoint: new(-21)},
		&journal.SMUReadback{Core: 0, Offset: -20},
		&journal.SMUReadback{Core: 0, Offset: 0},
		&journal.ProfileRestored{Offsets: []int{0, 0, 0}},
		&journal.Shutdown{Reason: journal.ShutdownSignal}))
	if s.stopped == nil || s.cores[0].applied != -20 {
		t.Fatalf("a stopped session shows the restored 0 instead of what was found: %+v", s.cores[0])
	}
}

func TestProjectSparseCoreIDs(t *testing.T) {
	t.Parallel()
	start := &journal.SessionStart{Session: "sparse", Cores: []machine.CoreInfo{{Core: 0, CCD: 0, CPUs: []int{0, 1}}, {Core: 8, CCD: 1, CPUs: []int{16, 17}}}}
	s := Project(dashboardEvents(start,
		&journal.ProfileRestored{Offsets: []int{-5, -9}},
		&journal.TrialIntent{Trial: "one", Condition: machine.Together, Phase: journal.PhaseChecking, Regime: machine.R1, Workload: "mprime-sse-4k-21k", Core: new(8), Profile: []int{-5, -9}, DurationS: 120},
		&journal.TrialEnd{Trial: "one", Outcome: journal.OutcomePass, DurationS: 120}))
	got := []string{fmt.Sprintf("core %02d applied %d", s.cores[1].id, s.cores[1].applied), s.history[len(s.history)-1].sentence()}
	want := []string{"core 08 applied -9", "light load on core 08 at -9, 2 min"}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("profiles list offsets in core order, not by core ID (-want +got):\n%s", diff)
	}
}

func TestProjectCrashClassification(t *testing.T) {
	t.Parallel()
	for _, tt := range []struct {
		name          string
		crash         *journal.CrashDetected
		wantCrashes   int
		wantLastCrash bool
	}{
		{name: "offset-related crash", crash: &journal.CrashDetected{PreviousBoot: "boot"}, wantCrashes: 1, wantLastCrash: true},
		{name: "power reset", crash: &journal.CrashDetected{PreviousBoot: "boot", Inconclusive: true}},
		{name: "before offsets", crash: &journal.CrashDetected{PreviousBoot: "boot", Stray: true}},
	} {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			s := Project(dashboardEvents(dashboardSession(), tt.crash))
			if s.crashes != tt.wantCrashes || (s.lastCrash != nil) != tt.wantLastCrash {
				t.Fatalf("crash classification lost: crashes=%d, lastCrash=%v", s.crashes, s.lastCrash)
			}
		})
	}
}

func TestProjectCrashEndsOnlyItsBootTrial(t *testing.T) {
	t.Parallel()
	for _, previousBoot := range []string{"boot", "older"} {
		t.Run(previousBoot, func(t *testing.T) {
			t.Parallel()
			events := dashboardEvents(dashboardSession(),
				&journal.TrialIntent{Trial: "one", Core: new(0), Offset: new(-25), Condition: machine.Alone, Phase: journal.PhaseSearch, Regime: machine.R1, Workload: "mprime-sse-4k-21k", DurationS: 90, Profile: []int{-25, 0, 0}},
				&journal.TrialStart{Trial: "one"},
				&journal.SMUIntent{Op: journal.SMUSet, Core: new(1), Offset: 0},
				&journal.CrashDetected{PreviousBoot: previousBoot, InFlight: new(4)})
			events[len(events)-1].Boot = "next"
			s := Project(events)
			running := previousBoot != "boot"
			if (s.trial != nil) != running || s.cores[0].loaded != running {
				t.Fatalf("trial from %q after reboot of %q: trial=%+v core=%+v", "boot", previousBoot, s.trial, s.cores[0])
			}
		})
	}
}

func TestProjectSMUDeadEndKeepsReadback(t *testing.T) {
	t.Parallel()
	s := Project(dashboardEvents(dashboardSession(),
		&journal.CorePhase{Core: 0, To: journal.PhaseSearch, Offset: -25},
		&journal.SMUIntent{Op: journal.SMUSet, Core: new(0), Offset: -25},
		&journal.SMUReadback{Core: 0, Offset: 0},
		&journal.DeadEnd{Condition: journal.DeadEndSMU, Detail: "core 00 read back 0 instead of -25"},
		&journal.Shutdown{Reason: journal.ShutdownDeadEnd}))
	if got := s.cores[0]; got.applied != 0 || got.tuned != -25 {
		t.Fatalf("SMU dead end replaced hardware readback with desired offset: %+v", got)
	}
}

func TestProjectTrialPreparationDoesNotClaimLoad(t *testing.T) {
	t.Parallel()
	events := dashboardEvents(dashboardSession(),
		&journal.TrialIntent{Trial: "one", Core: new(0), Offset: new(-25), Condition: machine.Alone, Phase: journal.PhaseSearch, Regime: machine.R1, Workload: "mprime-sse-4k-21k", DurationS: 90, Profile: []int{-25, 0, 0}},
		&journal.SMUReadback{Core: 0, Offset: -25})
	s := Project(events)
	if s.trial == nil || s.trial.hasStarted || s.cores[0].loaded || s.cores[0].tested || s.cores[0].applied != -25 {
		t.Fatalf("applied offsets during preparation are not a running workload: trial=%+v core=%+v", s.trial, s.cores[0])
	}
}
