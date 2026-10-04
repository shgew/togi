package watch

import (
	"strings"
	"testing"
	"time"

	"github.com/charmbracelet/x/ansi"
	"github.com/google/go-cmp/cmp"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func appendStoryEvents(events []journal.Event, payloads ...journal.Payload) []journal.Event {
	for _, p := range payloads {
		last := events[len(events)-1]
		events = append(events, journal.Event{
			Seq: last.Seq + 1, Time: last.Time.Add(time.Second), Boot: last.Boot,
			Kind: p.Kind(), Msg: p.Message(), Data: p,
		})
	}
	return events
}

func TestIdleTrialHoldsEveryViewUntilPlannedEnd(t *testing.T) {
	t.Parallel()
	events := cutTrial(t, simulated(t, sessionJournal), func(p *journal.TrialIntent) bool { return p.Regime == machine.R6 })
	s := Project(events)
	if s.trial == nil || !s.trial.hasStarted {
		t.Fatal("fixture must have a started idle trial")
	}
	until := s.trial.started.Add(s.trial.duration)
	for _, view := range []View{MainView, HelpView, LogView} {
		for _, size := range [][2]int{{240, 67}, {160, 45}, {120, 33}, {80, 24}} {
			sc := Screen{View: view, Width: size[0], Height: size[1], Keys: true, Scroll: -1}
			first := RenderView(s, sc, s.trial.started.Add(time.Second))
			later := RenderView(s, sc, until.Add(-time.Second))
			if diff := cmp.Diff(until, first.Until); diff != "" {
				t.Errorf("idle frame wake deadline (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff(first.Lines, later.Lines); diff != "" {
				t.Errorf("view %d %dx%d changes during idle trial (-first +later):\n%s", view, size[0], size[1], diff)
			}
			assertFrameBounds(t, first, sc)
			if view == MainView {
				text := ansi.Strip(strings.Join(first.Lines, "\n"))
				if strings.Contains(text, "left") {
					t.Errorf("idle trial shows a changing countdown:\n%s", text)
				}
				if !strings.Contains(text, until.Format("15:04")) {
					t.Errorf("idle trial hides planned end time:\n%s", text)
				}
			}
		}
	}
}

func TestTrialIntentDoesNotFreezeBeforeStart(t *testing.T) {
	t.Parallel()
	events := simulated(t, sessionJournal)
	intent := cutAt(t, events, func(e journal.Event) bool {
		p, ok := e.Data.(*journal.TrialIntent)
		return ok && p.Regime == machine.R6
	})
	s := Project(intent)
	if s.trial == nil || s.trial.hasStarted {
		t.Fatal("fixture must stop at idle trial intent, before its actual start")
	}
	drawn := RenderView(s, Screen{Width: 120, Height: 33}, cutTime(intent))
	if !drawn.Until.IsZero() {
		t.Errorf("unstarted trial freezes until %s", drawn.Until)
	}
	text := strings.ToLower(ansi.Strip(strings.Join(drawn.Lines, "\n")))
	if !strings.Contains(text, "waiting") && !strings.Contains(text, "starting") {
		t.Errorf("trial intent is presented as running:\n%s", text)
	}
	if strings.Contains(text, "offsets are set") {
		t.Errorf("an intent before its readback claims the offsets are set:\n%s", text)
	}
}

func TestBetweenTrialsNamesLastOutcomeInsteadOfCountdown(t *testing.T) {
	t.Parallel()
	events := simulated(t, sessionJournal)
	checking := cutTrial(t, events, func(p *journal.TrialIntent) bool { return p.Phase == journal.PhaseChecking })
	last := checking[len(checking)-1]
	ended := cutAt(t, events, func(e journal.Event) bool { return e.Seq > last.Seq && e.Kind == journal.KindTrialEnd })
	s := Project(ended)
	if s.trial != nil || s.last == nil {
		t.Fatal("fixture must stop between trials")
	}
	for _, size := range [][2]int{{240, 67}, {160, 45}, {120, 33}} {
		text := ansi.Strip(Render(s, size[0], size[1], cutTime(ended)))
		if !strings.Contains(text, "BETWEEN TRIALS") {
			t.Errorf("between-trial frame loses its state:\n%s", text)
		}
		if strings.Contains(text, "left") {
			t.Errorf("between-trial frame invents countdown:\n%s", text)
		}
		if !strings.Contains(strings.ToLower(text), "next:") && !strings.Contains(strings.ToLower(text), "deciding") {
			t.Errorf("between-trial frame hides tuner's next action:\n%s", text)
		}
	}
}

func TestStoppedFrameLabelsSavedNotAppliedOffsets(t *testing.T) {
	t.Parallel()
	events := simulated(t, sessionJournal)
	s := Project(events)
	if s.stopped == nil || !s.stopped.saved {
		t.Fatal("fixture must have a stopped run with restored hardware")
	}
	for _, size := range [][2]int{{240, 67}, {160, 45}, {120, 33}, {80, 24}} {
		text := strings.ToLower(ansi.Strip(Render(s, size[0], size[1], cutTime(events))))
		if !strings.Contains(text, "stopped") || !strings.Contains(text, "saved") || !strings.Contains(text, "not applied") && !strings.Contains(text, "not set") {
			t.Errorf("%dx%d stopped frame misrepresents restored hardware:\n%s", size[0], size[1], text)
		}
	}
}

func TestDeadEndFramePreservesRecordedCause(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		condition     journal.DeadEndCondition
		detail, wrong string
	}{
		{journal.DeadEndNoEvidence, "kernel log unreadable: permission denied", "installed"},
		{journal.DeadEndContainment, "termination unconfirmed", "termination confirmed"},
		{journal.DeadEndThermalTrip, "firmware reported a thermal trip", "wrong result"},
	} {
		s := Snapshot{session: true, start: time.Unix(1000, 0).UTC(), deadEnd: &deadEndView{at: time.Unix(1100, 0).UTC(), condition: tc.condition, detail: tc.detail}}
		text := ansi.Strip(Render(s, 240, 67, time.Unix(1200, 0).UTC()))
		if !strings.Contains(text, "DEAD END") || !strings.Contains(text, tc.detail) || strings.Contains(text, tc.wrong) {
			t.Errorf("dead end loses recorded cause or invents diagnosis:\n%s", text)
		}
	}
}

func TestDeepeningForecastKeepsTheProposedProfileApplied(t *testing.T) {
	t.Parallel()
	events := cutTrial(t, probeEvents(t), func(p *journal.TrialIntent) bool { return p.Phase == journal.PhaseDeepening })
	text := ansi.Strip(Render(Project(events), 240, 67, cutTime(events)))
	if !strings.Contains(text, "deepening round 1:") || strings.Contains(text, " alone at ") {
		t.Fatalf("deepening checks run with the proposed profile applied, not alone:\n%s", text)
	}
}
