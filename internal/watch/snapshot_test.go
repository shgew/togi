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
		&journal.HuntStart{Hunt: 3, Regime: machine.R7, Workload: "mprime-avx2-36k-248k-allcore", Cores: []int{0, 1, 2}, Parked: []int{-10, -30, -5}, Failing: []int{-20, -30, -10}, Candidates: []int{0, 2}, Trials: 5, TrialS: 120, DurationS: 120},
		&journal.HuntGroup{Hunt: 3, Group: 4, Cores: []int{2}, Profile: []int{-10, -30, -10}, DurationS: 120},
		&journal.TrialIntent{Trial: "group", Condition: machine.Parked, Phase: journal.PhaseHunt, Regime: machine.R7, Workload: "mprime-avx2-36k-248k-allcore", Cores: []int{0, 1, 2}, Profile: []int{-10, -30, -10}, DurationS: 120, Hunt: 3, Group: 4},
		&journal.TrialStart{Trial: "group"})
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
	if s.trial == nil || s.trial.id != "one" || !s.trial.hasStarted || s.trial.started != events[7].Time || s.trial.duration != 90*time.Second {
		t.Fatalf("an SMU write after the intent hid the running trial: %+v", s.trial)
	}
	if c := s.cores[0]; !c.loaded || c.applied != -25 || c.profile != -25 {
		t.Fatalf("target core not lit at its applied offset: %+v", c)
	}
	if c := s.cores[1]; c.loaded || c.state == coreParked {
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
	if s.hunt == nil || s.hunt.id != 3 || s.trial.workload.Base == "" {
		t.Fatalf("hunt identity lost: %+v", s.hunt)
	}
	for _, c := range s.cores {
		if !c.loaded || c.judged != (c.id == 2) {
			t.Fatalf("hunt loaded and judged roles lost: %+v", c)
		}
	}
}

func TestProjectMemberProbeRows(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name          string
		loaded        []int
		profile       []int
		started       bool
		heldOnly      bool
		groupAtParked bool
	}{
		{name: "all cores loaded", loaded: []int{0, 1, 2}, profile: []int{-15, -30, -8}, started: true},
		{name: "loaded noncandidate at nonparked offset", loaded: []int{0, 1, 2}, profile: []int{-15, -29, -8}, started: true},
		{name: "probe loaded alone", loaded: []int{0}, profile: []int{-15, -30, -8}, started: true},
		{name: "held member loaded alone", loaded: []int{2}, profile: []int{-15, -30, -8}, started: true},
		{name: "idle members judged while nonmember loaded", loaded: []int{1}, profile: []int{-15, -30, -8}, started: true},
		{name: "held member outside group cores", loaded: []int{0, 1, 2}, profile: []int{-15, -30, -8}, started: true, heldOnly: true},
		{name: "group member failing offset equals parked", loaded: []int{0, 1, 2}, profile: []int{-15, -30, -10}, started: true, groupAtParked: true},
		{name: "probe at parked offset", loaded: []int{0, 1, 2}, profile: []int{-10, -30, -8}, started: true},
		{name: "held member at parked offset", loaded: []int{0, 1, 2}, profile: []int{-15, -30, -5}, started: true},
		{name: "preparing member probe", loaded: []int{0, 1, 2}, profile: []int{-15, -30, -8}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			parked := []int{-10, -30, -5}
			if tc.groupAtParked {
				parked[2] = -10
			}
			groupCores := []int{2}
			if tc.heldOnly {
				groupCores = nil
			}
			held := []journal.CombinationMember{{Core: 2, Offset: tc.profile[2]}}
			if tc.groupAtParked {
				held = nil
			}
			events := dashboardEvents(dashboardSession(),
				&journal.CorePhase{Core: 0, To: journal.PhaseHasRoom, Offset: -20},
				&journal.CorePhase{Core: 1, To: journal.PhaseAtLimit, Offset: -30},
				&journal.CorePhase{Core: 2, To: journal.PhaseHasRoom, Offset: -10},
				&journal.ProfileChange{From: []int{0, 0, 0}, To: []int{-20, -30, -10}},
				&journal.HuntStart{Hunt: 1, Regime: machine.R7, Workload: "mprime-avx2-36k-248k-allcore", Cores: tc.loaded, Parked: parked, Failing: []int{-20, -30, -10}, Candidates: []int{0, 2}, Trials: 5, TrialS: 120, DurationS: 120},
				&journal.HuntGroup{Hunt: 1, Group: 1, Stage: "full", Cores: []int{0, 2}, Set: []int{0, 2}, Granularity: 2, Profile: []int{-20, -30, -10}, DurationS: 120},
				&journal.TrialIntent{Trial: "full", Condition: machine.Parked, Phase: journal.PhaseHunt, Regime: machine.R7, Workload: "mprime-avx2-36k-248k-allcore", Cores: tc.loaded, Profile: []int{-20, -30, -10}, DurationS: 120, Hunt: 1, Group: 1},
				&journal.TrialStart{Trial: "full"},
				&journal.TrialEnd{Trial: "full", Outcome: journal.OutcomeFailure, Signal: machine.ComputationError},
				&journal.HuntGroup{Hunt: 1, Group: 2, Stage: "probe", Set: []int{0, 2}, Granularity: 2, FullChecked: true, AnyFailed: true, Cores: groupCores, Probe: &journal.CombinationMember{Core: 0, Offset: tc.profile[0]}, Held: held, Profile: tc.profile, DurationS: 120},
				&journal.TrialIntent{Trial: "probe", Condition: machine.Parked, Phase: journal.PhaseHunt, Regime: machine.R7, Workload: "mprime-avx2-36k-248k-allcore", Cores: tc.loaded, Profile: tc.profile, DurationS: 120, Hunt: 1, Group: 2})
			if tc.started {
				events = appendStoryEvents(events, &journal.TrialStart{Trial: "probe"})
			}
			s := Project(events)
			for _, c := range s.cores {
				loaded := false
				for _, core := range tc.loaded {
					loaded = loaded || core == c.id
				}
				member := c.id == 0 || c.id == 2
				nonparked := c.id < len(tc.profile) && tc.profile[c.id] != parked[c.id]
				if c.loaded != (tc.started && loaded) || c.judged != (tc.started && (member || loaded && nonparked)) {
					t.Fatalf("member probe loaded and judged roles: %+v", c)
				}
			}
		})
	}
}

func TestFoldEntryFoldsRepeatedOutcomes(t *testing.T) {
	t.Parallel()
	at := time.Unix(0, 0).UTC()
	next := func() time.Time {
		at = at.Add(2 * time.Minute)
		return at
	}
	pass := func(first, peak int) entry {
		return entry{at: next(), tag: tagPass, kind: trialsEntry, key: "pass R7 full CCD 0 on 00-07", text: "R7 full CCD 0 on 00-07", runs: 1, first: first, of: 4, peak: new(peak)}
	}
	limit := func(core, offset int) entry {
		return entry{at: next(), tag: tagLimit, kind: limitsEntry, key: "limit", cores: []int{core}, offset: offset}
	}
	var history []entry
	for _, e := range []entry{
		pass(1, 70),
		pass(2, 76),
		pass(3, 62),
		{at: next(), tag: tagCrash, text: "R7 full CCD 0 on 00-07 · rebooted"},
		pass(4, 60),
		limit(12, -50),
		limit(14, -50),
		limit(3, -34),
	} {
		history = foldEntry(history, e)
	}
	var got []string
	for _, e := range history {
		got = append(got, e.at.Format("15:04")+" "+e.tag+": "+e.sentence())
	}
	if diff := cmp.Diff([]string{
		"00:06 pass: R7 full CCD 0 on 00-07 · trials 1-3 of 4 passed · Tctl max 76°C",
		"00:08 crash: R7 full CCD 0 on 00-07 · rebooted",
		"00:10 pass: R7 full CCD 0 on 00-07 · trial 4 of 4 passed · Tctl max 60°C",
		"00:14 limit: cores 12, 14 solo limit -50, the deepest offset",
		"00:16 limit: core 03 solo limit -34",
	}, got); diff != "" {
		t.Fatalf("adjacent outcomes of one part fold into one line with the latest time and the hottest peak, and nothing else folds (-want +got):\n%s", diff)
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
	if len(s.history) != historyLimit || s.history[0].text != fmt.Sprintf("warning %d", 2*historyLimit+4) {
		t.Fatalf("history not bounded to the newest events: %d entries, last %+v", len(s.history), s.history[len(s.history)-1])
	}
}

func TestProjectRawLogRecentSuffix(t *testing.T) {
	t.Parallel()
	for _, count := range []int{0, logLimit - 1, logLimit, logLimit + 1, 3*logLimit + 5} {
		t.Run(fmt.Sprint(count), func(t *testing.T) {
			t.Parallel()
			events := dashboardEvents(dashboardSession())
			for i := range count {
				batch := dashboardEvents(
					&journal.SMUIntent{Op: journal.SMUSet, Core: new(0), Offset: -20},
					&journal.SMUWrite{Core: new(0), Offset: -20},
					&journal.SMUReadback{Core: 0, Offset: -20},
					&journal.PreflightCheck{},
					&journal.SessionWarning{Operation: "projection", Error: fmt.Sprint(i)})
				for _, e := range batch {
					e.Seq = len(events) + 1
					e.Time = events[0].Time.Add(time.Duration(e.Seq) * time.Second)
					e.Msg = fmt.Sprintf("event %d: peak 75°C\n\x1b[31m", e.Seq)
					events = append(events, e)
				}
			}
			var want []entry
			for _, e := range events {
				if e.Kind == journal.KindSMUIntent || e.Kind == journal.KindSMUWrite || e.Kind == journal.KindSMUReadback || e.Kind == journal.KindPreflightCheck {
					continue
				}
				want = append(want, entry{at: e.Time, text: vtText(e.Msg)})
			}
			if len(want) > logLimit {
				want = want[len(want)-logLimit:]
			}
			got := Project(events).log
			for i := range got {
				got[i].tag = ""
			}
			if diff := cmp.Diff(want, got, cmp.AllowUnexported(entry{})); diff != "" {
				t.Fatalf("raw log must retain the same newest eligible events in journal order (-want +got):\n%s", diff)
			}
		})
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
	if s.stopped == nil || !s.stopped.saved || s.cores[0].applied != 0 || s.cores[0].profile != -20 {
		t.Fatalf("saved profile and hardware offsets were conflated: %+v", s.cores[0])
	}
}

func TestProjectSparseCoreIDs(t *testing.T) {
	t.Parallel()
	start := &journal.SessionStart{Session: "sparse", Cores: []machine.CoreInfo{{Core: 0, CCD: 0, CPUs: []int{0, 1}}, {Core: 8, CCD: 1, CPUs: []int{16, 17}}}}
	s := Project(dashboardEvents(start,
		&journal.ProfileRestored{Offsets: []int{-5, -9}},
		&journal.TrialIntent{Trial: "one", Condition: machine.Together, Phase: journal.PhaseChecking, Regime: machine.R1, Workload: "mprime-sse-4k-21k", Core: new(8), Profile: []int{-5, -9}, DurationS: 120},
		&journal.TrialEnd{Trial: "one", Outcome: journal.OutcomePass, DurationS: 120}))
	got := []string{fmt.Sprintf("core %02d applied %d", s.cores[1].id, s.cores[1].applied), s.history[0].sentence()}
	want := []string{"core 08 applied -9", "R1 light on core 08 at -9 · passed"}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("profiles list offsets in core order, not by core ID (-want +got):\n%s", diff)
	}
}

func TestProjectCrashClassification(t *testing.T) {
	t.Parallel()
	for _, tt := range []struct {
		name        string
		crash       *journal.CrashDetected
		wantCrashes int
	}{
		{name: "offset-related crash", crash: &journal.CrashDetected{PreviousBoot: "boot"}, wantCrashes: 1},
		{name: "power reset", crash: &journal.CrashDetected{PreviousBoot: "boot", Inconclusive: true}, wantCrashes: 1},
		{name: "before offsets", crash: &journal.CrashDetected{PreviousBoot: "boot", Stray: true}, wantCrashes: 1},
	} {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			s := Project(dashboardEvents(dashboardSession(), tt.crash))
			if s.crashes != tt.wantCrashes || s.recover == nil {
				t.Fatalf("unclean boot lost: crashes=%d recovery=%+v", s.crashes, s.recover)
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
	if got := s.cores[0]; got.applied != 0 || got.profile != -25 {
		t.Fatalf("SMU dead end replaced hardware readback with desired offset: %+v", got)
	}
}

func TestProjectTrialPreparationDoesNotClaimLoad(t *testing.T) {
	t.Parallel()
	events := dashboardEvents(dashboardSession(),
		&journal.TrialIntent{Trial: "one", Core: new(0), Offset: new(-25), Condition: machine.Alone, Phase: journal.PhaseSearch, Regime: machine.R1, Workload: "mprime-sse-4k-21k", DurationS: 90, Profile: []int{-25, 0, 0}},
		&journal.SMUReadback{Core: 0, Offset: -25})
	s := Project(events)
	if s.trial == nil || s.trial.hasStarted || s.cores[0].loaded || s.cores[0].judged || s.cores[0].applied != -25 {
		t.Fatalf("applied offsets during preparation are not a running workload: trial=%+v core=%+v", s.trial, s.cores[0])
	}
}

func TestProjectFailureCountersSeparateObservedAndSkipped(t *testing.T) {
	t.Parallel()
	events := dashboardEvents(dashboardSession(),
		&journal.CorePhase{Core: 0, To: journal.PhaseHasRoom, Offset: -20},
		&journal.Failure{Signal: machine.ComputationError, Attribution: journal.Attributed, Core: new(0), Offset: new(-20), Condition: machine.Alone},
		&journal.Failure{Signal: machine.ComputationError, Attribution: journal.Attributed, Core: new(0), Offset: new(-20), KnownFailure: 3},
		&journal.TrialIntent{Trial: "partial", Condition: machine.Together, Regime: machine.R7, Workload: "mprime-avx2-36k-248k-allcore", RecordOnly: true, Profile: []int{-19, 0, 0}, Cores: []int{0, 1}},
		&journal.TrialEnd{Trial: "partial", Outcome: journal.OutcomeFailure, Signal: machine.ComputationError})
	s := Project(events)
	if s.failures != 2 || s.lastFailure == nil || !s.lastFailure.Equal(events[2].Time) {
		t.Fatalf("observed failure and skipped evidence were conflated: count=%d last=%v", s.failures, s.lastFailure)
	}
	if s.history[0].tag != tagRecord {
		t.Fatalf("record-only outcome presented as a decision: %+v", s.history[0])
	}
}

func TestProjectRecoveryEndsOnlyWhenTrialStarts(t *testing.T) {
	t.Parallel()
	events := dashboardEvents(dashboardSession(),
		&journal.CrashDetected{PreviousBoot: "previous", Stray: true},
		&journal.TrialIntent{Trial: "next", Core: new(0), Regime: machine.R1, Workload: "mprime-sse-4k-21k", Condition: machine.Alone, Profile: []int{0, 0, 0}})
	if Project(events).recover == nil {
		t.Fatal("preparing offsets hid recovery before a trial started")
	}
	events = appendStoryEvents(events, &journal.TrialStart{Trial: "next"})
	if Project(events).recover != nil {
		t.Fatal("started trial retained recovery banner")
	}
}
