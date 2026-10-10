package watch

import (
	"strings"
	"testing"
	"time"

	"github.com/charmbracelet/x/ansi"
	"github.com/google/go-cmp/cmp"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
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

func TestIdleTrialHoldsEveryViewUntilItsEndIsRecorded(t *testing.T) {
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
			if !first.Until.After(until.Add(24 * time.Hour)) {
				t.Errorf("idle frame wakes at %s, before the trial's end can be recorded", first.Until)
			}
			for _, at := range []time.Time{until.Add(-time.Second), until.Add(time.Minute)} {
				if diff := cmp.Diff(first.Lines, RenderView(s, sc, at).Lines); diff != "" {
					t.Errorf("view %d %dx%d changes at %s during the idle trial (-first +later):\n%s", view, size[0], size[1], at, diff)
				}
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
	ended := cutPassedCheckingTrial(t, simulated(t, sessionJournal))
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

func TestDeepeningChecksSayHowTheyLoad(t *testing.T) {
	t.Parallel()
	alone := cutTrial(t, probeEvents(t), func(p *journal.TrialIntent) bool { return p.Phase == journal.PhaseDeepening })
	text := ansi.Strip(Render(Project(alone), 240, 67, cutTime(alone)))
	if !strings.Contains(text, "DEEPEN · ROUND 1 · CORE 00 AT -11 · LIGHT") || !strings.Contains(text, "Core 00 runs alone at its proposed -11") || strings.Contains(text, "solo limit, one turn at a time") {
		t.Fatalf("a deepened core's light check runs alone as part of its round, not as a solo-limit search:\n%s", text)
	}
	together := cutTrial(t, simulated(t, combinationJournal), func(p *journal.TrialIntent) bool {
		return p.Phase == journal.PhaseDeepening && p.Regime == machine.R7
	})
	text = ansi.Strip(Render(Project(together), 240, 67, cutTime(together)))
	if !strings.Contains(text, "DEEPEN · ROUND") || strings.Contains(text, " alone at ") {
		t.Fatalf("R7 deepening checks run with the proposed profile applied, not alone:\n%s", text)
	}
}

func TestSplitWordsParkAtZeroOnlyWhenTheProfileDoes(t *testing.T) {
	t.Parallel()
	s := Snapshot{order: []int{0, 1, 2}, hunt: &huntView{parkedZero: true, plan: []huntPart{{failing: []int{0}, parked: []int{1, 2}, running: true}}}, trial: &trialView{profile: []int{-10, 0, -30}}}
	if got := s.splitWords(); strings.Contains(got, "at 0") {
		t.Fatalf("core 02 stays at -30, yet the split says %q", got)
	}
	s.trial.profile = []int{-10, 0, 0}
	if got := s.splitWords(); !strings.Contains(got, "01 02 parked at 0") {
		t.Fatalf("split %q", got)
	}
}

func TestCarriedIdleFailureCauseIsNotATrial(t *testing.T) {
	t.Parallel()
	c := huntCause{known: true, carried: true}
	if got := c.knownWords(); strings.Contains(got, "trial") || !strings.Contains(got, "idle failure") {
		t.Fatalf("carried idle failure described as %q", got)
	}
}

func TestHuntCauseBriefAgreesWithNamedCore(t *testing.T) {
	t.Parallel()
	trial := trialView{regime: machine.R7, condition: machine.Together, cores: []int{0, 1}}
	for _, c := range []huntCause{{trial: trial, signal: machine.Crash}, {trial: trial, signal: machine.ComputationError}, {signal: machine.Crash}} {
		c.core = new(9)
		s := Snapshot{hunt: &huntView{cause: c}}
		_, named, brief := s.huntCauseStory()
		if !strings.HasPrefix(named, "Core 09 was named") || !strings.HasSuffix(brief, "; core 09 named.") {
			t.Errorf("full text %q but brief %q", named, brief)
		}
		c.core = nil
		s.hunt.cause = c
		if _, _, brief := s.huntCauseStory(); !strings.HasSuffix(brief, "; no core named.") {
			t.Errorf("unnamed brief %q", brief)
		}
	}
}

func TestStoryPartialExplainsOrdinaryEvidence(t *testing.T) {
	t.Parallel()
	s := Snapshot{session: true, trial: &trialView{hasStarted: true, regime: machine.R7, condition: machine.Together, cycle: 1, step: 1, partial: true, cores: []int{1}, parts: 2}}
	st := s.story(time.Time{}, false)
	text := strings.Join(st.lines, "\n")
	for _, want := range []string{"top-requester groups", "A backoff re-derives it", "an unchanged loaded set keeps its passes", "Passes and failures count as ordinary evidence"} {
		if !strings.Contains(text, want) {
			t.Errorf("partial narrative lost %q: %s", want, text)
		}
	}
}

func TestTightNextKeepsTrialTargets(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		regime machine.Regime
		kind   string
	}{
		{machine.R1, "light"},
		{machine.R2, "heavy vector"},
		{machine.R3, "load steps"},
		{machine.R4, "partial load"},
		{machine.R5, "both threads"},
		{machine.R6, "idle + bursts"},
		{machine.R7, "all-core"},
	} {
		t.Run(string(tc.regime), func(t *testing.T) {
			for _, target := range []struct {
				name      string
				condition machine.Condition
				cores     []int
				full      string
				short     string
			}{
				{"alone", machine.Alone, nil, "on core 03 alone at -16", "on core 03"},
				{"profile core", machine.Together, nil, "on core 03 at -16", "on core 03"},
				{"one loaded core", machine.Together, []int{3}, "on 03", "on 03"},
				{"core ranges", machine.Together, []int{0, 1, 2, 8, 9, 10, 15}, "on 00-02 08-10 15", "on 00-02 08-10 15"},
			} {
				t.Run(target.name, func(t *testing.T) {
					s := Snapshot{}
					next := tuner.Trial{Regime: tc.regime, Core: 3, Offset: -16, Condition: target.condition, Cores: target.cores}
					wantWords := string(tc.regime) + " " + tc.kind + " " + target.full
					if diff := cmp.Diff(wantWords, s.nextTrialWords(next, nil, huntShape{})); diff != "" {
						t.Fatalf("next trial description (-want +got):\n%s", diff)
					}
					_, compact, _ := s.outcomeWords(outcome{premise: ifPasses, next: &next})
					if diff := cmp.Diff("→ "+wantWords, compact); diff != "" {
						t.Fatalf("compact outcome (-want +got):\n%s", diff)
					}
					want := "→ " + string(tc.regime) + " " + target.short
					if diff := cmp.Diff(want, tightNext(compact)); diff != "" {
						t.Errorf("tight next trial target (-want +got):\n%s", diff)
					}
				})
			}
		})
	}
}

func TestTightNextLeavesNonCoreOutcomesUnchanged(t *testing.T) {
	t.Parallel()
	for _, text := range []string{
		"→ R2 heavy vector",
		"→ R6 idle + bursts",
		"→ R2 heavy vector on no cores",
		"→ step 3",
		"→ part 2",
		"→ group 4",
		"→ rerun",
		"→ no next trial projected",
	} {
		t.Run(text, func(t *testing.T) {
			if diff := cmp.Diff(text, tightNext(text)); diff != "" {
				t.Errorf("outcome without a core changed (-want +got):\n%s", diff)
			}
		})
	}
	t.Run("checking step consumer", func(t *testing.T) {
		s := Snapshot{}
		next := tuner.Trial{Regime: machine.R2, Core: 3, Offset: -16, Condition: machine.Together, Cycle: 1, Step: 3}
		_, compact, _ := s.outcomeWords(outcome{premise: ifPasses, next: &next})
		if diff := cmp.Diff("→ step 3", compact); diff != "" {
			t.Fatalf("checking step compact outcome (-want +got):\n%s", diff)
		}
		if diff := cmp.Diff(compact, tightNext(compact)); diff != "" {
			t.Errorf("checking step without a core changed (-want +got):\n%s", diff)
		}
	})
}
