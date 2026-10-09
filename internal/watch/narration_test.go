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
	running := Project(cutTrial(t, events, func(p *journal.TrialIntent) bool { return p.Phase == journal.PhaseChecking }))
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
