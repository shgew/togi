package watch

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/charmbracelet/x/ansi"
	"github.com/google/go-cmp/cmp"

	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/simrun"
	"github.com/shgew/togi/internal/watch/watchtest"
)

type watchCut struct {
	name   string
	events []journal.Event
}

func fixtureEvents(tb testing.TB, name string) []journal.Event {
	tb.Helper()
	dir := tb.TempDir()
	watchtest.Install(tb, dir, name)
	events, _, err := journal.ReadReplay(dir, 5)
	if err != nil {
		tb.Fatal(err)
	}
	return events
}

func cutAt(tb testing.TB, events []journal.Event, accept func(journal.Event) bool) []journal.Event {
	tb.Helper()
	for i, e := range events {
		if accept(e) {
			return events[:i+1]
		}
	}
	tb.Fatal("no cut point in the simulated journal")
	return nil
}

func cutTrial(tb testing.TB, events []journal.Event, accept func(*journal.TrialIntent) bool) []journal.Event {
	tb.Helper()
	trial := ""
	return cutAt(tb, events, func(e journal.Event) bool {
		switch p := e.Data.(type) {
		case *journal.TrialIntent:
			if trial == "" && accept(p) {
				trial = p.Trial
			}
		case *journal.TrialStart:
			return trial != "" && p.Trial == trial
		}
		return false
	})
}

func probeEvents(t *testing.T) []journal.Event {
	t.Helper()
	model := sim.DefaultModel()
	model.PastLimitRate = 1
	model.Signals = map[machine.Signal]float64{machine.Crash: 1}
	model.CrashMCE = 0
	limits := make([]sim.Limits, 16)
	for i := range limits {
		limits[i].Alone = [5]int{-50, -50, -50, -50, -50}
		limits[i].Together = [7]int{-50, -50, -50, -50, -50, -50, -50}
	}
	m, err := sim.New(sim.Config{
		Seed: 1, Cores: 16, Limits: limits, Model: &model,
		Joints: []sim.Joint{{Members: map[int]int{3: -10, 11: -10}, Regimes: []machine.Regime{machine.R7}, Rate: 10}},
	})
	if err != nil {
		t.Fatal(err)
	}
	cfg := config.Default()
	cfg.CandidateSoloLimits = make(map[int]int, 16)
	for core := range 16 {
		cfg.CandidateSoloLimits[core] = -10
	}
	dir := t.TempDir()
	deepening := false
	_, err = simrun.Simulate(context.Background(), simrun.Input{
		Config: cfg, ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Cycles: 1, InMemoryJournal: true,
		Until: func(e journal.Event) bool {
			switch p := e.Data.(type) {
			case *journal.TrialIntent:
				deepening = p.Phase == journal.PhaseDeepening
			case *journal.TrialStart:
				return deepening
			}
			return false
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	events, torn, err := journal.Read(dir)
	if err != nil || torn != nil {
		t.Fatalf("read simulated journal: %v, torn %q", err, torn)
	}
	return events
}

func watchCuts(t *testing.T) []watchCut {
	t.Helper()
	events := fixtureEvents(t, "concluded")
	checking := cutTrial(t, events, func(p *journal.TrialIntent) bool {
		return p.Phase == journal.PhaseChecking && p.Regime == machine.R7
	})
	last := checking[len(checking)-1]
	deadEnd := &journal.DeadEnd{Condition: journal.DeadEndNoEvidence, Detail: "five trials in a row proved nothing"}
	probes := probeEvents(t)
	probing := false
	memberProbe := cutAt(t, probes, func(e journal.Event) bool {
		if p, ok := e.Data.(*journal.HuntGroup); ok && p.Probe != nil {
			probing = true
		}
		return probing && e.Kind == journal.KindTrialStart
	})
	return []watchCut{
		{"search", cutTrial(t, events, func(p *journal.TrialIntent) bool {
			return p.Phase == journal.PhaseSearch && p.Offset != nil && *p.Offset < 0
		})},
		{"confirm", cutTrial(t, probes, func(p *journal.TrialIntent) bool { return p.Phase == journal.PhaseSearch })},
		{"checking", checking},
		{"hunt", cutTrial(t, fixtureEvents(t, "hunt"), func(p *journal.TrialIntent) bool { return p.Phase == journal.PhaseHunt })},
		{"member-probe", memberProbe},
		{"deepening", cutTrial(t, probes, func(p *journal.TrialIntent) bool { return p.Phase == journal.PhaseDeepening })},
		{"idle", cutTrial(t, events, func(p *journal.TrialIntent) bool { return p.Regime == machine.R6 })},
		{"between", cutAt(t, events, func(e journal.Event) bool { return e.Seq > last.Seq && e.Kind == journal.KindTrialEnd })},
		{"recovering", cutAt(t, events, func(e journal.Event) bool { return e.Kind == journal.KindCrashDetected })},
		{"stopped", events},
		{"combination", fixtureEvents(t, "combination")},
		{"deadend", slices.Concat(checking, []journal.Event{
			{Seq: last.Seq + 1, Time: last.Time.Add(time.Minute), Boot: last.Boot, Kind: deadEnd.Kind(), Msg: deadEnd.Message(), Data: deadEnd},
		})},
	}
}

func cutTime(events []journal.Event) time.Time {
	return events[len(events)-1].Time.Add(40 * time.Second).UTC()
}

func assertFrameBounds(t *testing.T, drawn Drawn, sc Screen) {
	t.Helper()
	if diff := cmp.Diff(max(0, sc.Height-1), len(drawn.Lines)); diff != "" {
		t.Errorf("frame rows (-want +got):\n%s", diff)
	}
	for row, line := range drawn.Lines {
		if cells := ansi.StringWidth(line); cells > max(0, sc.Width-1) {
			t.Errorf("%dx%d: row %d writes reserved column (%d cells): %q", sc.Width, sc.Height, row, cells, ansi.Strip(line))
		}
	}
}

func TestWatchFrames(t *testing.T) {
	cuts := watchCuts(t)
	for _, c := range cuts {
		t.Run(c.name, func(t *testing.T) {
			s, now := Project(c.events), cutTime(c.events)
			for _, size := range [][2]int{{240, 67}, {160, 45}, {120, 33}} {
				sc := Screen{Width: size[0], Height: size[1], Keys: true}
				drawn := RenderView(s, sc, now)
				assertFrameBounds(t, drawn, sc)
				golden(t, fmt.Sprintf("watch-%s-%dx%d", c.name, size[0], size[1]), ansi.Strip(strings.Join(drawn.Lines, "\n"))+"\n")
			}
			for _, view := range []View{MainView, HelpView, LogView} {
				for _, scroll := range []int{0, 3, -1, 100000} {
					sc := Screen{View: view, Scroll: scroll, Width: 80, Height: 24, Keys: true}
					assertFrameBounds(t, RenderView(s, sc, now), sc)
				}
			}
			if c.name == "checking" || c.name == "member-probe" || c.name == "idle" {
				golden(t, "watch-"+c.name+"-240x67-color", Render(s, 240, 67, now)+"\n")
			}
			if c.name == "member-probe" {
				for _, view := range []struct {
					name   string
					view   View
					scroll int
				}{{"help", HelpView, 0}, {"help-end", HelpView, -1}, {"log", LogView, -1}} {
					for _, size := range [][2]int{{240, 67}, {160, 45}, {80, 24}} {
						sc := Screen{View: view.view, Scroll: view.scroll, Width: size[0], Height: size[1], Keys: true}
						drawn := RenderView(s, sc, now)
						assertFrameBounds(t, drawn, sc)
						golden(t, fmt.Sprintf("view-%s-%dx%d", view.name, size[0], size[1]), ansi.Strip(strings.Join(drawn.Lines, "\n"))+"\n")
					}
				}
			}
		})
	}
	now := time.Unix(0, 0).UTC()
	for _, s := range []Snapshot{{}, {problem: errors.New("journal is unreadable")}} {
		for _, view := range []View{MainView, HelpView, LogView} {
			sc := Screen{View: view, Width: 80, Height: 24, Keys: true}
			assertFrameBounds(t, RenderView(s, sc, now), sc)
		}
	}
}

func TestDashboardEscapesDiagnosticControls(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 9, 30, 12, 0, 0, 0, time.UTC)
	msg := "日本語\x1b[2J\x1b]52;c;data\x07\r\n\u009b31m\u009d0;title\u009c\u2028\u2029"
	escaped := `\u65e5\u672c\u8a9e\x1b[2J\x1b]52;c;data\x07\r\n\u009b31m\u009d0;title\u009c\u2028\u2029`
	start := journal.Event{Seq: 1, Time: now, Kind: journal.KindSessionStart, Data: &journal.SessionStart{Session: "diagnostics"}}
	for _, tt := range []struct {
		name string
		data journal.Payload
	}{
		{"recent backend retry", &journal.BackendRetry{}},
		{"dead end", &journal.DeadEnd{Condition: journal.DeadEndContainment, Detail: msg}},
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
	if !strings.Contains(frame, `\u65e5\u672c\u8a9e\x1b]0;title\x07\r\n\u009b2J`) {
		t.Fatalf("trial workload not visibly escaped: %q", frame)
	}
	if strings.Contains(frame, "\x1b]0;") || strings.ContainsAny(frame, "\r\u009b") {
		t.Fatalf("trial workload controls in dashboard: %q", frame)
	}
}

func TestWatchWithoutJournal(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	now := time.Unix(0, 0).UTC()
	for _, size := range [][2]int{{240, 67}, {160, 45}, {120, 33}} {
		golden(t, fmt.Sprintf("watch-missing-%dx%d", size[0], size[1]), ansi.Strip(Render(Load(dir), size[0], size[1], now))+"\n")
		golden(t, fmt.Sprintf("watch-problem-%dx%d", size[0], size[1]), ansi.Strip(Render(Snapshot{problem: errors.New("read journal: permission denied")}, size[0], size[1], now))+"\n")
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

func BenchmarkProject(b *testing.B) {
	events := fixtureEvents(b, "concluded")
	events = cutTrial(b, events, func(p *journal.TrialIntent) bool { return p.Phase == journal.PhaseChecking && p.Regime == machine.R7 })
	b.ReportAllocs()
	b.ResetTimer()
	for b.Loop() {
		Project(events)
	}
}
