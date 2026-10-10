package watch

import (
	"errors"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/charmbracelet/x/ansi"
	"github.com/google/go-cmp/cmp"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

// words joins a frame's text on single spaces, so a sentence wrapped over rows still matches.
func words(frame string) string {
	return strings.Join(strings.Fields(strings.ReplaceAll(ansi.Strip(frame), "▌", "")), " ")
}

func TestNoTrialYetDoesNotClaimOneEnded(t *testing.T) {
	t.Parallel()
	events := simulated(t, sessionJournal)
	first := slices.IndexFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindTrialIntent })
	if first < 1 {
		t.Fatal("fixture must record a trial intent after session.start")
	}
	for _, tc := range []struct {
		name   string
		events []journal.Event
		want   string
	}{
		{"session start to readback", events[:first], "No trial has run yet"},
		{"first trial ended", cutAt(t, events, func(e journal.Event) bool { return e.Kind == journal.KindTrialEnd }), "The last trial has ended"},
	} {
		s := Project(tc.events)
		if s.trial != nil || (tc.want == "No trial has run yet") != (s.last == nil) {
			t.Fatalf("%s: fixture is not between trials as intended: trial %v, last %v", tc.name, s.trial, s.last)
		}
		for _, size := range [][2]int{{240, 67}, {160, 45}, {120, 33}} {
			text := words(Render(s, size[0], size[1], cutTime(tc.events)))
			if !strings.Contains(text, tc.want) {
				t.Errorf("%s at %dx%d lacks %q:\n%s", tc.name, size[0], size[1], tc.want, text)
			}
			if tc.want == "No trial has run yet" && strings.Contains(text, "has ended") {
				t.Errorf("%s at %dx%d claims a trial ended:\n%s", tc.name, size[0], size[1], text)
			}
		}
	}
}

func TestCompleteConfirmationTurnRecordsTheSoloLimit(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name         string
		light, heavy int
		want         string
	}{
		{"first light", 0, 0, "confirm: light trial 1 of 5"},
		{"last light", 4, 0, "confirm: light trial 5 of 5"},
		{"first heavy", 5, 0, "confirm: heavy trial 1 of 5"},
		{"last heavy", 5, 4, "confirm: heavy trial 5 of 5"},
		{"complete", 5, 5, "records the solo limit"},
	} {
		s := Snapshot{
			session: true,
			cores:   []coreView{{id: 11, state: coreConfirm, confirm: &confirmView{offset: -7, light: tc.light, heavy: tc.heavy, needed: 5}}},
			turns:   []turnView{{core: 11, confirm: true, offset: -7, regimes: []machine.Regime{machine.R1}}},
		}
		for _, class := range []sizeClass{wideLayout, mediumLayout} {
			text := ansi.Strip(strings.Join(s.turnLines(tableColumns(class, 80), 80, class), "\n"))
			if !strings.Contains(text, tc.want) {
				t.Errorf("%s, class %d: turn row lacks %q:\n%s", tc.name, class, tc.want, text)
			}
		}
	}
}

func TestStoppedStageLineShowsNoRunningStage(t *testing.T) {
	t.Parallel()
	events := simulated(t, sessionJournal)
	stopped := Project(events)
	if stopped.stopped == nil {
		t.Fatal("fixture must end stopped")
	}
	stopped.stopped.reason = journal.ShutdownCycles
	running := Project(cutWhen(t, simulated(t, concludedJournal), func(s *tuner.State, e journal.Event) bool {
		return e.Kind == journal.KindTrialStart && s.PhasePlan().Phase == 0
	}))
	if running.stopped != nil {
		t.Fatal("fixture must be running")
	}
	now := cutTime(events)
	for _, class := range []sizeClass{wideLayout, compactLayout} {
		line := ansi.Strip(stopped.stageLine(class, false, now))
		if strings.Contains(line, "►") || strings.Contains(line, "repeats until stopped") {
			t.Errorf("class %d: stopped stage line shows a running stage: %q", class, line)
		}
	}
	line := ansi.Strip(running.stageLine(wideLayout, false, now))
	if !strings.Contains(line, "►") || !strings.Contains(line, "repeats until stopped") {
		t.Errorf("running stage line lost its marker or repeat note: %q", line)
	}
	if text := words(Render(stopped, 240, 67, now)); !strings.Contains(text, "The requested clean cycles are complete") {
		t.Errorf("stopped narration does not say why the session stopped:\n%s", text)
	}
}

func TestUnreadableJournalIsNotAMissingOne(t *testing.T) {
	t.Parallel()
	reason := "journal line 1: invalid character 'g'"
	unreadable := Snapshot{problem: errors.New(reason)}
	live := words(strings.Join(RenderView(unreadable, Screen{Width: 120, Height: 10}, time.Unix(0, 0).UTC()).Lines, "\n"))
	once := words(Render(unreadable, 120, 10, time.Unix(0, 0).UTC()))
	for name, text := range map[string]string{"live": live, "one frame": once} {
		if !strings.Contains(text, "can't read journal: "+reason) {
			t.Errorf("%s frame lacks the reason:\n%s", name, text)
		}
		if strings.Contains(text, "no session yet") {
			t.Errorf("%s frame calls an unreadable journal a missing one:\n%s", name, text)
		}
	}
	if !strings.Contains(live, "I'll retry when the journal changes") {
		t.Errorf("live frame lacks the retry promise:\n%s", live)
	}
	if strings.Contains(once, "retry") {
		t.Errorf("one-frame print promises a retry:\n%s", once)
	}
	missing := words(Render(Snapshot{}, 120, 10, time.Unix(0, 0).UTC()))
	if !strings.Contains(missing, "no session yet") || strings.Contains(missing, "can't read journal") {
		t.Errorf("missing journal frame:\n%s", missing)
	}
}

func TestLoadTellsMissingFromUnreadable(t *testing.T) {
	t.Parallel()
	missing := t.TempDir()
	unreadable := t.TempDir()
	if err := os.WriteFile(filepath.Join(unreadable, "events.jsonl"), []byte("{garbage\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]bool{false, true}, []bool{Load(missing).Err() != nil, Load(unreadable).Err() != nil}); diff != "" {
		t.Fatalf("Err of a missing, then an unreadable journal (-want +got):\n%s", diff)
	}
}

func TestCrashedTrialNotYetRecoveredIsNotNarratedAsRunning(t *testing.T) {
	t.Parallel()
	events := beforeCrashDetected(t, simulated(t, sessionJournal))
	s := Project(events)
	if s.trial == nil || !s.trial.crashed || s.recover != nil {
		t.Fatalf("fixture must end on an unfinished trial a later boot outlived: trial %+v, recover %v", s.trial, s.recover)
	}
	now := cutTime(events)
	if line := ansi.Strip(s.stageLine(wideLayout, true, now)); strings.Contains(line, "►") || strings.Contains(line, "left") || !strings.Contains(line, "trial crashed") {
		t.Errorf("stage line of a crashed trial: %q", line)
	}
	for _, size := range [][2]int{{240, 67}, {160, 45}, {120, 33}} {
		frame := Render(s, size[0], size[1], now)
		text := words(frame)
		for _, want := range []string{"crashed", "togi run records the crash", "trial " + s.trial.id} {
			if !strings.Contains(text, want) {
				t.Errorf("%dx%d lacks %q:\n%s", size[0], size[1], want, text)
			}
		}
		for _, bad := range []string{"left", "past its end", "I'm finding", "I'm confirming"} {
			if strings.Contains(text, bad) {
				t.Errorf("%dx%d still says %q for a crashed trial:\n%s", size[0], size[1], bad, text)
			}
		}
	}
	// A trial whose boot is still the journal's latest is running, not crashed.
	started := Project(cutTrial(t, simulated(t, sessionJournal), func(p *journal.TrialIntent) bool { return p.Phase == journal.PhaseSearch }))
	if started.trial == nil || started.trial.crashed {
		t.Fatalf("a trial in the latest boot is running: %+v", started.trial)
	}
}

func TestTrialPastItsPlannedEndShowsHowLongWithoutClaimingACrash(t *testing.T) {
	t.Parallel()
	events := cutTrial(t, simulated(t, sessionJournal), func(p *journal.TrialIntent) bool {
		return p.Phase == journal.PhaseSearch && p.Regime != machine.R6
	})
	s := Project(events)
	tr := s.trial
	if tr == nil || tr.crashed || !tr.hasStarted || tr.duration <= 0 {
		t.Fatalf("fixture must end on a started trial in the latest boot: %+v", tr)
	}
	end := tr.started.Add(tr.duration)
	for _, size := range [][2]int{{240, 67}, {160, 45}, {120, 33}} {
		sc := Screen{View: MainView, Width: size[0], Height: size[1]}
		before := words(strings.Join(RenderView(s, sc, end.Add(-time.Second)).Lines, "\n"))
		if !strings.Contains(before, "left") || strings.Contains(before, "past") {
			t.Errorf("%dx%d before the planned end:\n%s", size[0], size[1], before)
		}
		drawn := RenderView(s, sc, end.Add(42*time.Second))
		text := words(strings.Join(drawn.Lines, "\n"))
		if !strings.Contains(text, "0:42") || !strings.Contains(text, "past") || !strings.Contains(text, "may still be running") {
			t.Errorf("%dx%d lacks the time past the end and the reason it is no crash:\n%s", size[0], size[1], text)
		}
		for _, bad := range []string{"0:00 left", "crashed", "togi run records", "CRASHED"} {
			if strings.Contains(text, bad) {
				t.Errorf("%dx%d says %q past the planned end with nothing following:\n%s", size[0], size[1], bad, text)
			}
		}
		if !drawn.Until.IsZero() {
			t.Errorf("%dx%d: frame past the end holds still until %s; live watch must keep ticking", size[0], size[1], drawn.Until)
		}
	}
	line := ansi.Strip(s.stageLine(wideLayout, true, end.Add(42*time.Second)))
	if !strings.Contains(line, "0:42 past its end") {
		t.Errorf("stage line lacks the time past the end: %q", line)
	}
}

func TestProjectPriorRecoveryDoesNotMaskLaterBootCrash(t *testing.T) {
	t.Parallel()
	events := dashboardEvents(dashboardSession(),
		&journal.CorePhase{Core: 0, To: journal.PhaseSearch, Offset: -25},
		&journal.TrialIntent{Trial: "trial-A", Core: new(0), Offset: new(-25), Condition: machine.Alone, Phase: journal.PhaseSearch, Regime: machine.R1, Workload: "mprime-sse-4k-21k", DurationS: 90, Profile: []int{-25, 0, 0}},
		&journal.TrialStart{Trial: "trial-A"})
	for i := range events {
		events[i].Boot = "boot-A"
	}
	reboot := len(events)
	events = appendStoryEvents(events,
		&journal.ConfigLoaded{},
		&journal.CrashDetected{PreviousBoot: "boot-A", InFlight: new(3)},
		&journal.TrialEnd{Trial: "trial-A", Outcome: journal.OutcomeFailure, Signal: machine.Crash},
		&journal.TrialIntent{Trial: "trial-B", Core: new(0), Offset: new(-20), Condition: machine.Alone, Phase: journal.PhaseSearch, Regime: machine.R1, Workload: "mprime-sse-4k-21k", DurationS: 90, Profile: []int{-20, 0, 0}})
	for i := reboot; i < len(events); i++ {
		events[i].Boot = "boot-B"
	}
	preparing := Project(events)
	if preparing.trial == nil || preparing.trial.id != "trial-B" || preparing.trial.hasStarted || preparing.trial.crashed {
		t.Fatalf("next intent must be preparing on the recovery boot: %+v", preparing.trial)
	}
	if r := preparing.recover; r == nil || r.trial == nil || r.trial.id != "trial-A" || r.end == nil || r.end.id != "trial-A" {
		t.Fatalf("same-boot intent must retain trial A's recorded recovery: %+v", r)
	}
	events = appendStoryEvents(events, &journal.ConfigLoaded{})
	events[len(events)-1].Boot = "boot-C"
	s := Project(events)
	if s.trial == nil || s.trial.id != "trial-B" || !s.trial.crashed || s.trial.hasStarted {
		t.Fatalf("boot C must establish that unstarted trial B crashed: %+v", s.trial)
	}
	now := cutTime(events)
	for _, oneFrame := range []bool{false, true} {
		if st := s.story(now, oneFrame); st.label != "CRASHED" || !strings.Contains(st.brief, "Trial trial-B crashed") {
			t.Errorf("oneFrame=%t: prior recovery masks trial B in the story: %+v", oneFrame, st)
		}
	}
	if title, _, lines := s.restingBand(); title != "CRASHED, NOT YET RECOVERED" || !strings.Contains(strings.Join(lines, " "), "trial trial-B") {
		t.Errorf("prior recovery masks trial B in NOW: %q, %v", title, lines)
	}
	for _, size := range [][2]int{{240, 67}, {160, 45}, {120, 33}} {
		live := RenderView(s, Screen{View: MainView, Width: size[0], Height: size[1]}, now)
		for name, frame := range map[string]string{"live": strings.Join(live.Lines, "\n"), "one frame": Render(s, size[0], size[1], now)} {
			text := words(frame)
			for _, want := range []string{"CRASHED, NOT YET RECOVERED", "trial trial-B", "togi run records the crash"} {
				if !strings.Contains(text, want) {
					t.Errorf("%s %dx%d lacks %q:\n%s", name, size[0], size[1], want, text)
				}
			}
			if strings.Contains(text, "RECOVERED FROM A CRASH") || strings.Contains(text, "I picked up from the journal after the reboot") {
				t.Errorf("%s %dx%d still presents trial A's recovery as current:\n%s", name, size[0], size[1], text)
			}
		}
	}
}

func TestNarrationResetAfterRebootShowsUnrecoveredCrash(t *testing.T) {
	t.Parallel()
	events := cutTrial(t, simulated(t, sessionJournal), func(p *journal.TrialIntent) bool {
		return p.Phase == journal.PhaseSearch && p.Regime != machine.R6
	})
	running := Project(events)
	if running.trial == nil || !running.trial.hasStarted || running.trial.crashed || running.recover != nil {
		t.Fatalf("fixture must have a started, unrecovered trial: %+v", running.trial)
	}
	command := len(events)
	events = appendStoryEvents(events,
		&journal.CommandReset{Core: new(running.trial.core)},
		&journal.Shutdown{Reason: journal.ShutdownCommand})
	for i := command; i < len(events); i++ {
		events[i].Boot = "reset-boot"
	}
	events[len(events)-1].Cause = []int{events[command].Seq}
	s := Project(events)
	if s.trial == nil || !s.trial.crashed || !s.trial.hasStarted || s.recover != nil || s.stopped == nil || s.stopped.reason != journal.ShutdownCommand {
		t.Fatalf("reset boot must stop cleanly without recovering the old crashed trial: trial=%+v recover=%+v stopped=%+v", s.trial, s.recover, s.stopped)
	}
	now := cutTime(events)
	for _, oneFrame := range []bool{false, true} {
		if st := s.story(now, oneFrame); st.label != "CRASHED" || !strings.Contains(st.brief, "Trial "+s.trial.id+" crashed") {
			t.Errorf("oneFrame=%t: command shutdown masks the unrecovered trial in the story: %+v", oneFrame, st)
		}
	}
	if title, _, lines := s.restingBand(); title != "CRASHED, NOT YET RECOVERED" || !strings.Contains(strings.Join(lines, " "), "trial "+s.trial.id) {
		t.Errorf("command shutdown masks the unrecovered trial in NOW: %q, %v", title, lines)
	}
	if line := ansi.Strip(s.stageLine(wideLayout, true, now)); strings.Contains(line, "►") || strings.Contains(line, "left") || !strings.Contains(line, "trial crashed") {
		t.Errorf("reset after reboot presents a running stage: %q", line)
	}
	for _, size := range [][2]int{{240, 67}, {160, 45}, {120, 33}} {
		live := RenderView(s, Screen{View: MainView, Width: size[0], Height: size[1]}, now)
		for name, frame := range map[string]string{"live": strings.Join(live.Lines, "\n"), "one frame": Render(s, size[0], size[1], now)} {
			text := words(frame)
			for _, want := range []string{"CRASHED, NOT YET RECOVERED", "trial " + s.trial.id, "togi run records the crash"} {
				if !strings.Contains(text, want) {
					t.Errorf("%s %dx%d lacks %q:\n%s", name, size[0], size[1], want, text)
				}
			}
			for _, bad := range []string{"STOPPED", "left", "past its end", "RECOVERED FROM A CRASH"} {
				if strings.Contains(text, bad) {
					t.Errorf("%s %dx%d says %q instead of the unrecovered crash:\n%s", name, size[0], size[1], bad, text)
				}
			}
		}
	}
}

func TestNarrationOverdueWithProgressReportsMissingTrialEnd(t *testing.T) {
	t.Parallel()
	events := cutTrial(t, simulated(t, sessionJournal), func(p *journal.TrialIntent) bool {
		return p.Phase == journal.PhaseSearch && p.Regime != machine.R6
	})
	running := Project(events)
	if running.trial == nil || !running.trial.hasStarted || running.trial.crashed || running.trial.duration <= 0 {
		t.Fatalf("fixture must have a started non-R6 trial: %+v", running.trial)
	}
	end := running.trial.started.Add(running.trial.duration)
	// Output collection during teardown can record progress after the planned end.
	events = appendStoryEvents(events, &journal.TrialProgress{Trial: running.trial.id, Detail: "FFT 36K done"})
	events[len(events)-1].Time = end.Add(10 * time.Second)
	s := Project(events)
	now := end.Add(42 * time.Second)
	if s.trial == nil || s.trial.crashed || !s.trial.hasStarted || s.trial.pastEnd(now) != 42*time.Second || s.recover != nil || s.stopped != nil {
		t.Fatalf("same-boot progress must leave an overdue, not crashed, trial: %+v", s.trial)
	}
	for _, oneFrame := range []bool{false, true} {
		st := s.story(now, oneFrame)
		text := words(strings.Join(st.lines, "\n"))
		if !strings.Contains(text, "no trial end recorded") || strings.Contains(text, "nothing recorded since") {
			t.Errorf("oneFrame=%t: overdue story misrepresents later progress:\n%s", oneFrame, text)
		}
	}
	for _, size := range [][2]int{{240, 67}, {160, 45}, {120, 33}} {
		live := RenderView(s, Screen{View: MainView, Width: size[0], Height: size[1]}, now)
		if !live.Until.IsZero() {
			t.Errorf("%dx%d: non-R6 overdue frame holds still until %s", size[0], size[1], live.Until)
		}
		for name, frame := range map[string]string{"live": strings.Join(live.Lines, "\n"), "one frame": Render(s, size[0], size[1], now)} {
			text := words(frame)
			for _, want := range []string{"0:42", "past", "no trial end recorded", "may still be running"} {
				if !strings.Contains(text, want) {
					t.Errorf("%s %dx%d lacks %q:\n%s", name, size[0], size[1], want, text)
				}
			}
			for _, bad := range []string{"nothing recorded since", "0:00 left", "crashed", "CRASHED", "togi run records the crash"} {
				if strings.Contains(text, bad) {
					t.Errorf("%s %dx%d says %q despite same-boot progress:\n%s", name, size[0], size[1], bad, text)
				}
			}
		}
	}
}

func TestNarrationR6OverdueKeepsQuietFrameUntilReboot(t *testing.T) {
	t.Parallel()
	events := cutTrial(t, simulated(t, sessionJournal), func(p *journal.TrialIntent) bool { return p.Regime == machine.R6 })
	s := Project(events)
	if s.trial == nil || !s.trial.hasStarted || s.trial.crashed || s.trial.duration <= 0 || s.recover != nil {
		t.Fatalf("fixture must have a started R6 trial in the latest boot: %+v", s.trial)
	}
	end := s.trial.started.Add(s.trial.duration)
	firstAt := s.trial.started.Add(time.Second)
	for _, size := range [][2]int{{240, 67}, {160, 45}, {120, 33}, {80, 24}} {
		for _, oneFrame := range []bool{false, true} {
			sc := Screen{View: MainView, Width: size[0], Height: size[1], OneFrame: oneFrame, Keys: !oneFrame}
			first := RenderView(s, sc, firstAt)
			if !first.Until.Equal(heldUntilRecorded) {
				t.Errorf("oneFrame=%t %dx%d: R6 must hold until an event, not its planned end: %s", oneFrame, size[0], size[1], first.Until)
			}
			text := words(strings.Join(first.Lines, "\n"))
			if !strings.Contains(text, end.Format("15:04")) {
				t.Errorf("oneFrame=%t %dx%d: frozen R6 frame lacks planned end:\n%s", oneFrame, size[0], size[1], text)
			}
			for _, at := range []time.Time{end.Add(-time.Second), end.Add(42 * time.Second), end.Add(time.Hour)} {
				later := RenderView(s, sc, at)
				if diff := cmp.Diff(first, later); diff != "" {
					t.Errorf("oneFrame=%t %dx%d: R6 frame or hold changes at %s (-first +later):\n%s", oneFrame, size[0], size[1], at, diff)
				}
				text := words(strings.Join(later.Lines, "\n"))
				for _, bad := range []string{"left", "past its", "past the", "past its planned", "crashed", "CRASHED", "togi run records the crash"} {
					if strings.Contains(text, bad) {
						t.Errorf("oneFrame=%t %dx%d: same-boot R6 says %q at %s:\n%s", oneFrame, size[0], size[1], bad, at, text)
					}
				}
				if diff := cmp.Diff(Render(s, size[0], size[1], firstAt), Render(s, size[0], size[1], at)); diff != "" {
					t.Errorf("%dx%d: one-frame R6 print changes at %s (-first +later):\n%s", size[0], size[1], at, diff)
				}
			}
		}
	}
	config := slices.IndexFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindConfigLoaded })
	if config < 0 {
		t.Fatal("fixture must record the configuration before the idle trial")
	}
	events = appendStoryEvents(events, events[config].Data)
	events[len(events)-1].Boot = "after-idle"
	events[len(events)-1].Time = end.Add(42 * time.Second)
	crashed := Project(events)
	if crashed.trial == nil || !crashed.trial.crashed || !crashed.trial.hasStarted || crashed.trial.regime != machine.R6 || crashed.recover != nil {
		t.Fatalf("a later boot must establish an unrecovered R6 crash: %+v", crashed.trial)
	}
	if until := crashed.quietUntil(); !until.IsZero() {
		t.Errorf("a crashed R6 trial still freezes the frame until %s", until)
	}
	now := events[len(events)-1].Time.Add(time.Second)
	if line := ansi.Strip(crashed.stageLine(wideLayout, true, now)); strings.Contains(line, "►") || strings.Contains(line, "left") || !strings.Contains(line, "trial crashed") {
		t.Errorf("crashed R6 stage line still runs or counts down: %q", line)
	}
	for _, size := range [][2]int{{240, 67}, {160, 45}, {120, 33}, {80, 24}} {
		for _, oneFrame := range []bool{false, true} {
			drawn := RenderView(crashed, Screen{View: MainView, Width: size[0], Height: size[1], OneFrame: oneFrame}, now)
			if !drawn.Until.IsZero() {
				t.Errorf("oneFrame=%t %dx%d: crashed R6 retains its quiet hold: %s", oneFrame, size[0], size[1], drawn.Until)
			}
			text := words(strings.Join(drawn.Lines, "\n"))
			for _, want := range []string{"CRASHED, NOT YET RECOVERED", "trial " + crashed.trial.id, "togi run records the crash"} {
				if !strings.Contains(text, want) {
					t.Errorf("oneFrame=%t %dx%d: crashed R6 lacks %q:\n%s", oneFrame, size[0], size[1], want, text)
				}
			}
			for _, bad := range []string{"left", "past its", "screen paused until", "RECOVERED FROM A CRASH"} {
				if strings.Contains(text, bad) {
					t.Errorf("oneFrame=%t %dx%d: crashed R6 still says %q:\n%s", oneFrame, size[0], size[1], bad, text)
				}
			}
		}
	}
}
