package watch

import (
	"context"
	"errors"
	"fmt"
	"os"
	"regexp"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/charmbracelet/x/ansi"
	"github.com/google/go-cmp/cmp"

	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/simrun"
)

type watchCut struct {
	name   string
	events []journal.Event
}

// simulate runs a simulated session through its first clean cycle, or until until reports true, and returns its
// journal at the current ruleset. Its config.loaded events name a fixed build, so a release's version bump leaves the
// frames unchanged.
func simulate(m *sim.Machine, cfg config.Config, until func(journal.Event) bool) ([]journal.Event, error) {
	dir, err := os.MkdirTemp("", "togi-watch-frames")
	if err != nil {
		return nil, err
	}
	defer os.RemoveAll(dir)
	_, err = simrun.Simulate(context.Background(), simrun.Input{
		Config: cfg, ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Cycles: 1, InMemoryJournal: true, Until: until,
	})
	if err != nil {
		return nil, err
	}
	events, torn, err := journal.Read(dir)
	if err != nil || torn != nil {
		return nil, fmt.Errorf("read simulated journal: %w, torn %q", err, torn)
	}
	for i, e := range events {
		if p, ok := e.Data.(*journal.ConfigLoaded); ok {
			p.Version, p.Rev = "0.0.0", "dev"
			events[i].Msg = p.Message()
		}
	}
	return events, nil
}

// sessionJournal is a default simulated machine through its first clean cycle: search, checking with R6, hunts,
// crash recoveries and the stop.
var sessionJournal = sync.OnceValues(func() ([]journal.Event, error) {
	m, err := sim.New(sim.Config{Seed: 1})
	if err != nil {
		return nil, err
	}
	return simulate(m, config.Default(), nil)
})

// probeMachine fails together only through a combination of cores 03 and 11 in R6, where a failure naming no core
// still starts a hunt, so its hunt probes members.
func probeMachine() (*sim.Machine, config.Config, error) {
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
		Joints: []sim.Joint{{Members: map[int]int{3: -10, 11: -10}, Regimes: []machine.Regime{machine.R6}, Rate: 10}},
	})
	cfg := config.Default()
	cfg.CandidateSoloLimits = make(map[int]int, 16)
	for core := range 16 {
		cfg.CandidateSoloLimits[core] = -10
	}
	return m, cfg, err
}

// probeJournal runs the probe machine until its first deepening trial starts.
var probeJournal = sync.OnceValues(func() ([]journal.Event, error) {
	m, cfg, err := probeMachine()
	if err != nil {
		return nil, err
	}
	deepening := false
	return simulate(m, cfg, func(e journal.Event) bool {
		switch p := e.Data.(type) {
		case *journal.TrialIntent:
			deepening = p.Phase == journal.PhaseDeepening
		case *journal.TrialStart:
			return deepening
		}
		return false
	})
})

// combinationJournal runs the probe machine to its stop, with the combination it found.
var combinationJournal = sync.OnceValues(func() ([]journal.Event, error) {
	m, cfg, err := probeMachine()
	if err != nil {
		return nil, err
	}
	return simulate(m, cfg, nil)
})

// simulated returns a copy of a simulated journal, so a test can append to it.
func simulated(tb testing.TB, run func() ([]journal.Event, error)) []journal.Event {
	tb.Helper()
	events, err := run()
	if err != nil {
		tb.Fatal(err)
	}
	return slices.Clone(events)
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
	return simulated(t, probeJournal)
}

func watchCuts(t *testing.T) []watchCut {
	t.Helper()
	events := simulated(t, sessionJournal)
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
		{"hunt", cutTrial(t, simulated(t, sessionJournal), func(p *journal.TrialIntent) bool { return p.Phase == journal.PhaseHunt })},
		{"member-probe", memberProbe},
		{"deepening", cutTrial(t, probes, func(p *journal.TrialIntent) bool { return p.Phase == journal.PhaseDeepening })},
		{"idle", cutTrial(t, events, func(p *journal.TrialIntent) bool { return p.Regime == machine.R6 })},
		{"between", cutAt(t, events, func(e journal.Event) bool { return e.Seq > last.Seq && e.Kind == journal.KindTrialEnd })},
		{"crashed", beforeCrashDetected(t, events)},
		{"recovering", cutAt(t, events, func(e journal.Event) bool { return e.Kind == journal.KindCrashDetected })},
		{"stopped", events},
		{"combination", simulated(t, combinationJournal)},
		{"deadend", slices.Concat(checking, []journal.Event{
			{Seq: last.Seq + 1, Time: last.Time.Add(time.Minute), Boot: last.Boot, Kind: deadEnd.Kind(), Msg: deadEnd.Message(), Data: deadEnd},
		})},
	}
}

// beforeCrashDetected is the journal a crashed session shows after the machine restarted and before togi run records
// the crash: it ends with the events a later boot wrote first.
func beforeCrashDetected(tb testing.TB, events []journal.Event) []journal.Event {
	tb.Helper()
	i := slices.IndexFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindCrashDetected })
	if i < 1 {
		tb.Fatal("no crash in the simulated journal")
	}
	return events[:i]
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

// allSizeGoldens names the cuts whose main frame is compared at every size: each draws a size-dependent panel that
// no other kept cut draws. The other cuts are compared at the widest size only; every cut still renders at every
// size under assertFrameBounds.
var allSizeGoldens = map[string]bool{
	"search": true, "checking": true, "hunt": true, "member-probe": true,
	"deepening": true, "idle": true, "crashed": true, "recovering": true, "combination": true,
}

func TestWatchFrames(t *testing.T) {
	cuts := watchCuts(t)
	for _, c := range cuts {
		t.Run(c.name, func(t *testing.T) {
			s, now := Project(c.events), cutTime(c.events)
			for i, size := range [][2]int{{240, 67}, {160, 45}, {120, 33}} {
				sc := Screen{Width: size[0], Height: size[1], Keys: true}
				drawn := RenderView(s, sc, now)
				assertFrameBounds(t, drawn, sc)
				if i == 0 || allSizeGoldens[c.name] {
					golden(t, fmt.Sprintf("watch-%s-%dx%d", c.name, size[0], size[1]), ansi.Strip(strings.Join(drawn.Lines, "\n"))+"\n")
				}
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

// A styled line costs at most one style change per cell; styles that pile up without text between them, as cutting
// and rejoining styled strings once did, break this long before any terminal shows a difference.
func TestFramesSpendBytesOnCellsNotStaleStyles(t *testing.T) {
	redundant := regexp.MustCompile(`\x1b\[[0-9;]*m\x1b\[0?m`)
	for _, c := range watchCuts(t) {
		s, now := Project(c.events), cutTime(c.events)
		for _, size := range [][2]int{{240, 67}, {160, 45}, {120, 33}, {80, 24}} {
			for _, view := range []View{MainView, HelpView, LogView} {
				sc := Screen{View: view, Width: size[0], Height: size[1], Keys: true}
				for row, line := range RenderView(s, sc, now).Lines {
					if limit := 16*ansi.StringWidth(line) + 16; len(line) > limit {
						t.Errorf("%s %dx%d view %d row %d: %d bytes for %d cells", c.name, size[0], size[1], view, row, len(line), ansi.StringWidth(line))
					}
					if at := redundant.FindStringIndex(line); at != nil {
						t.Errorf("%s %dx%d view %d row %d: style reset before use: %q", c.name, size[0], size[1], view, row, line[at[0]:at[1]])
					}
				}
			}
		}
	}
}

// Every glyph the dashboard itself draws must be in the console font; a glyph outside it would reach the console as a
// visible \u escape.
func TestFramesDrawOnlyIBM437Glyphs(t *testing.T) {
	for _, c := range watchCuts(t) {
		s, now := Project(c.events), cutTime(c.events)
		for _, size := range [][2]int{{240, 67}, {160, 45}, {120, 33}, {80, 24}} {
			for _, view := range []View{MainView, HelpView, LogView} {
				for row, line := range RenderView(s, Screen{View: view, Width: size[0], Height: size[1], Keys: true}, now).Lines {
					text := ansi.Strip(line)
					for _, ch := range text {
						if ch >= 128 && !strings.ContainsRune(ibm437, ch) {
							t.Errorf("%s %dx%d view %d row %d draws %q outside IBM437: %q", c.name, size[0], size[1], view, row, ch, text)
						}
					}
					if strings.Contains(text, `\u`) {
						t.Errorf("%s %dx%d view %d row %d escapes a glyph the console cannot draw: %q", c.name, size[0], size[1], view, row, text)
					}
				}
			}
		}
	}
}

func TestConsoleTextReplacesWhatTheFontCannotDraw(t *testing.T) {
	t.Parallel()
	for in, want := range map[string]string{
		"5 × 2m":           "5 x 2m",
		"parts…":           "parts...",
		"a — b – c":        "a - b - c",
		"‘a’ “b”":          `'a' "b"`,
		"░▄█ ·►":           "░▄█ ·►",
		"Tctl 95°C":        "Tctl 95°C",
		"done ✓ 日":         `done \u2713 \u65e5`,
		"plain ascii text": "plain ascii text",
	} {
		if got := consoleText(in); got != want {
			t.Errorf("consoleText(%q) = %q, want %q", in, got, want)
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
		unreadable := Snapshot{problem: errors.New("read journal: permission denied")}
		golden(t, fmt.Sprintf("watch-problem-%dx%d", size[0], size[1]), ansi.Strip(strings.Join(RenderView(unreadable, Screen{Width: size[0], Height: size[1]}, now).Lines, "\n"))+"\n")
		golden(t, fmt.Sprintf("watch-problem-once-%dx%d", size[0], size[1]), ansi.Strip(Render(unreadable, size[0], size[1], now))+"\n")
	}
}

func TestWatchBetweenTrialMCE(t *testing.T) {
	t.Parallel()
	events := simulated(t, sessionJournal)
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
	events := simulated(b, sessionJournal)
	events = cutTrial(b, events, func(p *journal.TrialIntent) bool { return p.Phase == journal.PhaseChecking && p.Regime == machine.R7 })
	b.ReportAllocs()
	b.ResetTimer()
	for b.Loop() {
		Project(events)
	}
}

// With colour off the trial bar still shows how far the trial is: elapsed cells are █ and the rest ░, as the gauges
// use shape as well as colour.
func TestTrialBarShowsElapsedWithoutColour(t *testing.T) {
	var events []journal.Event
	for _, c := range watchCuts(t) {
		if c.name == "search" {
			events = c.events
		}
	}
	s, early := Project(events), cutTime(events)
	late := early.Add(30 * time.Second)
	for _, size := range [][2]int{{240, 67}, {160, 45}, {120, 33}} {
		sc := Screen{Width: size[0], Height: size[1], Keys: true}
		bar := func(now time.Time) string {
			var rows []string
			for _, line := range RenderView(s, sc, now).Lines {
				if text := strings.TrimSpace(ansi.Strip(line)); strings.HasPrefix(text, "█") {
					rows = append(rows, text[:strings.IndexFunc(text, func(r rune) bool { return r != '█' && r != '░' })])
				}
			}
			if len(rows) == 0 {
				t.Fatalf("%dx%d: no trial bar row in the frame", size[0], size[1])
			}
			return rows[0]
		}
		before, after := bar(early), bar(late)
		if before == after {
			t.Errorf("%dx%d: colour-off bar is identical at two elapsed times: %q", size[0], size[1], before)
		}
		if strings.Count(after, "█") <= strings.Count(before, "█") || !strings.Contains(before, "░") {
			t.Errorf("%dx%d: elapsed cells must grow as █ over ░ rest: %q then %q", size[0], size[1], before, after)
		}
	}
}
