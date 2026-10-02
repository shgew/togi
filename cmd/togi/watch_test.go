package main

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/watch"
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
	renderFixture(t, dir, "concluded")
	events, _, err := journal.Read(dir)
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
	guard := through(func(e journal.Event) bool {
		p, ok := e.Data.(*journal.GuardRotation)
		return ok && p.Event == journal.RotationStart
	})
	last := events[len(events)-1]
	p := &journal.DeadEnd{Condition: journal.DeadEndNoEvidence, Detail: "five trials in a row proved nothing"}
	deadEnd := slices.Concat(events, []journal.Event{
		{Seq: last.Seq + 1, Time: last.Time.Add(time.Minute), Boot: last.Boot, Kind: journal.KindDeadEnd, Msg: p.Message(), Data: p},
	})
	all := [][2]int{{240, 67}, {160, 45}, {120, 33}}
	return []watchCut{
		{name: "search", events: search, sizes: all, color: true},
		{name: "guard", events: guard, sizes: all, color: true},
		{name: "deadend", events: deadEnd, sizes: all[:1]},
	}
}

func cutTime(events []journal.Event) time.Time {
	return events[len(events)-1].Time.Add(40 * time.Second).UTC()
}

func TestWatchFrames(t *testing.T) {
	t.Parallel()
	for _, c := range watchCuts(t) {
		s, now := watch.Project(c.events), cutTime(c.events)
		for _, size := range c.sizes {
			frame := watch.Render(s, size[0], size[1], now)
			golden(t, fmt.Sprintf("watch-%s-%dx%d", c.name, size[0], size[1]), ansi.Strip(frame)+"\n")
		}
		if c.color {
			golden(t, fmt.Sprintf("watch-%s-240x67-color", c.name), watch.Render(s, 240, 67, now)+"\n")
		}
	}
}

func TestWatchFrameFitsScreen(t *testing.T) {
	t.Parallel()
	for _, c := range watchCuts(t) {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			s, now := watch.Project(c.events), cutTime(c.events)
			for w := 1; w <= 300; w += 7 {
				t.Run(fmt.Sprint(w), func(t *testing.T) {
					t.Parallel()
					for h := 2; h <= 100; h += 5 {
						lines := strings.Split(watch.Render(s, w, h, now), "\n")
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

func TestWatchWithoutJournal(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	var stdout, stderr bytes.Buffer
	if code := cli([]string{"--state-dir", dir, "watch", "--width", "120", "--height", "33"}, &stdout, &stderr); code != exitOK {
		t.Fatalf("exit %d, stderr %q", code, stderr.String())
	}
	if !strings.Contains(stdout.String(), "no session yet") {
		t.Errorf("stdout %q, want it to say no session yet", stdout.String())
	}
	if stderr.Len() != 0 {
		t.Errorf("stderr %q, want empty", stderr.String())
	}
	golden(t, "watch-missing-120x33", ansi.Strip(watch.Render(watch.Load(dir), 120, 33, time.Unix(0, 0).UTC()))+"\n")
	if code := cli([]string{"--state-dir", dir, "watch", "--width", "0"}, &stdout, &stderr); code != exitUsage {
		t.Errorf("--width 0: exit %d, want %d", code, exitUsage)
	}
}

func TestWatchProblemFrame(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		fixture string
		problem string
	}{
		{name: "malformed", problem: "invalid character"},
		{name: "incompatible-schema", fixture: "schema", problem: "uses schema 2"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			var dir string
			if tc.fixture != "" {
				dir, _ = incompatibleFixture(t, tc.fixture)
			} else {
				dir = t.TempDir()
				if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte("not json\n"), 0o644); err != nil {
					t.Fatal(err)
				}
			}
			var stdout, stderr bytes.Buffer
			code := cli([]string{"--state-dir", dir, "watch", "--width", "120", "--height", "33"}, &stdout, &stderr)
			if code != exitError {
				t.Errorf("exit %d, want %d", code, exitError)
			}
			if !strings.Contains(strings.Join(strings.Fields(stdout.String()), " "), tc.problem) {
				t.Errorf("stdout %q, want problem %q", stdout.String(), tc.problem)
			}
			if !strings.Contains(stderr.String(), "togi watch: ") || !strings.Contains(stderr.String(), tc.problem) {
				t.Errorf("stderr %q, want watch error %q", stderr.String(), tc.problem)
			}
		})
	}
}

func TestWatchEscapesJournalControls(t *testing.T) {
	t.Parallel()
	dir, original := incompatibleFixture(t, "schema")
	data := bytes.ReplaceAll(original, []byte("0.2.1"), []byte(`\u001b[31m0.2.1\u001b[0m\u001b]52;c;data\u0007\r\n\u009b2J\u2028`))
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), data, 0o644); err != nil {
		t.Fatal(err)
	}
	var stdout, stderr bytes.Buffer
	if code := cli([]string{"--state-dir", dir, "watch"}, &stdout, &stderr); code != exitError {
		t.Fatalf("exit %d, want %d", code, exitError)
	}
	for name, text := range map[string]string{"stdout": stdout.String(), "stderr": stderr.String()} {
		if strings.ContainsAny(text, "\x1b\r\u009b\u2028") || !strings.Contains(text, `\x1b[31m0.2.1\x1b[0m\x1b]52;c;data\x07\r\n\u009b2J\u2028+def5678`) {
			t.Errorf("%s %q, want visibly escaped diagnostic controls", name, text)
		}
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
		{"last failure", &journal.Failure{}},
		{"dead end", &journal.DeadEnd{Condition: journal.DeadEndContainment}},
		{"in flight", &journal.SMUIntent{}},
	} {
		t.Run(tt.name, func(t *testing.T) {
			events := []journal.Event{start, {Seq: 2, Time: now, Kind: tt.data.Kind(), Msg: msg, Data: tt.data}}
			frame := watch.Render(watch.Project(events), 300, 40, now)
			if !strings.Contains(frame, escaped) {
				t.Fatalf("diagnostic not visibly escaped:\n%q", frame)
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

func TestStatusEscapesJournalMessages(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 9, 30, 12, 0, 0, 0, time.UTC)
	msg := "日本語\x1b[2J\x1b]52;c;data\x07\r\n\u009b31m\u2028"
	escaped := `日本語\x1b[2J\x1b]52;c;data\x07\r\n\u009b31m\u2028`
	st := journal.State{
		Session:  &journal.SessionInfo{ID: "diagnostics", Start: now},
		InFlight: &journal.InFlight{Seq: 2, Msg: msg},
		Cores:    []journal.CoreState{{Core: 0, LastDecision: &journal.DecisionRef{Seq: 5, Msg: msg}}},
	}
	var out bytes.Buffer
	writeStatus(&out, st, []journal.Event{
		{Seq: 3, Kind: journal.KindSessionCarried, Msg: msg},
		{Seq: 4, Kind: journal.KindMCE, Msg: msg, Data: &journal.MCE{BetweenTrials: true}},
	})
	for _, prefix := range []string{"in flight: [#2] ", "carried: [#3] ", "between-trial evidence [#4]: ", "[#5] "} {
		if !strings.Contains(out.String(), prefix+escaped) {
			t.Fatalf("status diagnostic %q not visibly escaped: %q", prefix, out.String())
		}
	}
	if strings.ContainsAny(out.String(), "\x1b\r\u009b\u2028") {
		t.Fatalf("status contains executable controls: %q", out.String())
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
	frame := watch.Render(watch.Project(events), 300, 40, now)
	if !strings.Contains(frame, `日本語\x1b]0;title\x07\r\n\u009b2J`) {
		t.Fatalf("trial workload not visibly escaped: %q", frame)
	}
	if strings.Contains(frame, "\x1b]0;") || strings.ContainsAny(frame, "\r\u009b") {
		t.Fatalf("trial workload controls in dashboard: %q", frame)
	}
}
