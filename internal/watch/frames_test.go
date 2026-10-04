package watch

import (
	"fmt"
	"slices"
	"strings"
	"testing"
	"time"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/watch/watchtest"
)

type watchCut struct {
	name   string
	events []journal.Event
	sizes  [][2]int
	color  bool
}

func watchCuts(t *testing.T) []watchCut {
	t.Helper()
	dir := t.TempDir()
	watchtest.Install(t, dir, "concluded")
	events, _, err := journal.ReadReplay(dir, 5)
	if err != nil {
		t.Fatal(err)
	}
	through := func(first func(journal.Event) bool) []journal.Event {
		t.Helper()
		found := false
		for i, e := range events {
			if !found {
				found = first(e)
				continue
			}
			if _, ok := e.Data.(*journal.TrialStart); ok {
				return events[:i+1]
			}
		}
		t.Fatal("no cut point in the simulated journal")
		return nil
	}
	search := through(func(e journal.Event) bool {
		p, ok := e.Data.(*journal.TunerDecision)
		return ok && p.Phase == journal.PhaseSearch && p.Decision == journal.Backoff
	})
	checking := through(func(e journal.Event) bool {
		p, ok := e.Data.(*journal.CheckingCycle)
		return ok && p.Event == journal.CycleStart
	})
	last := events[len(events)-1]
	p := &journal.DeadEnd{Condition: journal.DeadEndNoEvidence, Detail: "five trials in a row proved nothing"}
	deadEnd := slices.Concat(events, []journal.Event{
		{Seq: last.Seq + 1, Time: last.Time.Add(time.Minute), Boot: last.Boot, Kind: journal.KindDeadEnd, Msg: p.Message(), Data: p},
	})
	all := [][2]int{{240, 67}, {160, 45}, {120, 33}}
	return []watchCut{
		{name: "search", events: search, sizes: all, color: true},
		{name: "checking", events: checking, sizes: all, color: true},
		{name: "deadend", events: deadEnd, sizes: all[:1]},
	}
}

func cutTime(events []journal.Event) time.Time {
	return events[len(events)-1].Time.Add(40 * time.Second).UTC()
}

func TestWatchFrames(t *testing.T) {
	t.Parallel()
	for _, c := range watchCuts(t) {
		s, now := Project(c.events), cutTime(c.events)
		for _, size := range c.sizes {
			frame := Render(s, size[0], size[1], now)
			golden(t, fmt.Sprintf("watch-%s-%dx%d", c.name, size[0], size[1]), ansi.Strip(frame)+"\n")
		}
		if c.color {
			golden(t, fmt.Sprintf("watch-%s-240x67-color", c.name), Render(s, 240, 67, now)+"\n")
		}
	}
}

func TestWatchFrameFitsScreen(t *testing.T) {
	t.Parallel()
	for _, c := range watchCuts(t) {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			s, now := Project(c.events), cutTime(c.events)
			for w := 1; w <= 300; w += 7 {
				t.Run(fmt.Sprint(w), func(t *testing.T) {
					t.Parallel()
					for h := 2; h <= 100; h += 5 {
						lines := strings.Split(Render(s, w, h, now), "\n")
						if len(lines) != h-1 {
							t.Errorf("%dx%d: %d lines, want %d", w, h, len(lines), h-1)
						}
						for i, ln := range lines {
							if lipgloss.Width(ln) > w-1 {
								t.Errorf("%dx%d: line %d is %d cells wide, want at most %d", w, h, i, lipgloss.Width(ln), w-1)
							}
						}
					}
				})
			}
		})
	}
}

func TestDashboardEscapesDiagnosticControls(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 9, 30, 12, 0, 0, 0, time.UTC)
	msg := "日本語\x1b[2J\x1b]52;c;data\x07\r\n\u009b31m\u009d0;title\u009c\u2028\u2029"
	escaped := `日本語\x1b[2J\x1b]52;c;data\x07\r\n\u009b31m\u009d0;title\u009c\u2028\u2029`
	start := journal.Event{Seq: 1, Time: now, Kind: journal.KindSessionStart, Data: &journal.SessionStart{Session: "diagnostics"}}
	for _, tt := range []struct {
		name string
		data journal.Payload
	}{
		{"recent backend retry", &journal.BackendRetry{}},
		{"dead end", &journal.DeadEnd{Condition: journal.DeadEndContainment}},
		{"in flight", &journal.SMUIntent{}},
	} {
		t.Run(tt.name, func(t *testing.T) {
			events := []journal.Event{start, {Seq: 2, Time: now, Kind: tt.data.Kind(), Msg: msg, Data: tt.data}}
			frame := Render(Project(events), 300, 40, now)
			if !strings.Contains(strings.Join(strings.Fields(ansi.Strip(frame)), ""), escaped) {
				t.Fatalf("diagnostic not visibly escaped, even across wrapped lines:\n%q", frame)
			}
			for _, control := range []string{"\x1b[2J", "\x1b]52;", "\r", "\u009b", "\u009d", "\u009c", "\u2028", "\u2029"} {
				if strings.Contains(frame, control) {
					t.Fatalf("untrusted control %q in dashboard: %q", control, frame)
				}
			}
			if got := strings.Count(frame, "\n"); got != 38 {
				t.Fatalf("diagnostic changed frame line count: %d, want 38", got)
			}
			if events[1].Msg != msg {
				t.Fatalf("projection sanitized raw message: %q", events[1].Msg)
			}
		})
	}
}

func TestDashboardEscapesTrialDetails(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 9, 30, 12, 0, 0, 0, time.UTC)
	events := []journal.Event{
		{Seq: 1, Time: now, Kind: journal.KindSessionStart, Data: &journal.SessionStart{Session: "diagnostics"}},
		{Seq: 2, Time: now, Kind: journal.KindTrialIntent, Msg: "trial running", Data: &journal.TrialIntent{
			Trial: "0001", Workload: "日本語\x1b]0;title\x07\r\n\u009b2J", DurationS: 60,
		}},
	}
	frame := Render(Project(events), 300, 40, now)
	if !strings.Contains(frame, `日本語\x1b]0;title\x07\r\n\u009b2J`) {
		t.Fatalf("trial workload not visibly escaped: %q", frame)
	}
	if strings.Contains(frame, "\x1b]0;") || strings.ContainsAny(frame, "\r\u009b") {
		t.Fatalf("trial workload controls in dashboard: %q", frame)
	}
}

func TestWatchWithoutJournal(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	golden(t, "watch-missing-120x33", ansi.Strip(Render(Load(dir), 120, 33, time.Unix(0, 0).UTC()))+"\n")
}

func TestWatchCombinationAndOpenHunt(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name  string
		until journal.Kind
	}{
		{name: "hunt", until: journal.KindHuntGroup},
		{name: "combination"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			dir := t.TempDir()
			watchtest.Install(t, dir, tc.name)
			events, _, err := journal.ReadReplay(dir, 5)
			if err != nil {
				t.Fatal(err)
			}
			frameEvents := events
			if tc.until != "" {
				groupStarted := false
				for i, e := range events {
					if e.Kind == journal.KindHuntGroup {
						groupStarted = true
					}
					if groupStarted && e.Kind == journal.KindTrialStart {
						frameEvents = events[:i+1]
						break
					}
				}
			}
			frame := Render(Project(frameEvents), 240, 67, frameEvents[len(frameEvents)-1].Time.Add(40*time.Second))
			golden(t, "watch-"+tc.name+"-240x67", ansi.Strip(frame)+"\n")
		})
	}
}

func TestWatchBetweenTrialMCE(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	watchtest.Install(t, dir, "concluded")
	events, _, err := journal.ReadReplay(dir, 5)
	if err != nil {
		t.Fatal(err)
	}
	p := &journal.MCE{CPU: 0, Core: 0, Corrected: true, BetweenTrials: true, Lines: []string{"between-trial hardware error"}}
	last := events[len(events)-1]
	e := journal.Event{Seq: last.Seq + 1, Time: last.Time.Add(time.Minute), Boot: "between-trials", Kind: p.Kind(), Msg: p.Message(), Data: p}
	events = append(events, e)
	frame := ansi.Strip(Render(Project(events), 240, 67, e.Time))
	if !strings.Contains(frame, "between trials (recorded only)") {
		t.Fatalf("watch hides between-trial MCE: %s", frame)
	}
}
