package watch

import (
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
)

func dashboardCheckingEvents(extra ...journal.Payload) []journal.Event {
	return dashboardEvents(append([]journal.Payload{dashboardSession(),
		&journal.CorePhase{Core: 0, To: journal.PhaseAtLimit, Offset: -20, FailurePoint: new(-21)},
		&journal.CorePhase{Core: 1, To: journal.PhaseAtLimit, Offset: -30, FailurePoint: new(-31)},
		&journal.CorePhase{Core: 2, To: journal.PhaseAtLimit, Offset: -50},
		&journal.ProfileChange{To: []int{-20, -30, -50}},
		&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{machine.R1, machine.R2, machine.R7}},
		&journal.TrialIntent{Trial: "checking", Condition: machine.Together, Phase: journal.PhaseChecking, Regime: machine.R2, Workload: "mprime-avx2-36k-248k", Core: new(1), Profile: []int{-20, -30, -50}, DurationS: 120, Cycle: 1},
		&journal.TrialStart{Trial: "checking"},
	}, extra...)...)
}

func TestDashboardActivityFrames(t *testing.T) {
	t.Parallel()
	now := time.Unix(1100, 0).UTC()
	for _, tc := range []struct {
		name          string
		events        []journal.Event
		width, height int
	}{
		{"search", dashboardEvents(dashboardSession(),
			&journal.CorePhase{Core: 0, To: journal.PhaseSearch, Offset: -25, Pass: new(-20), FailurePoint: new(-40)},
			&journal.TrialIntent{Trial: "search", Core: new(0), Offset: new(-25), Condition: machine.Alone, Phase: journal.PhaseSearch, Regime: machine.R1, Workload: "mprime-sse-4k-21k", DurationS: 90, Profile: []int{-25, 0, 0}},
			&journal.SMUReadback{Core: 0, Offset: -25},
			&journal.TrialStart{Trial: "search"}), 200, 60},
		{"hunt", dashboardHuntEvents(), 200, 60},
		{"deepening", dashboardDeepeningEvents(), 200, 60},
		{"checking", dashboardCheckingEvents(), 200, 60},
		{"narrow", dashboardCheckingEvents(), 90, 60},
		{"dead-end", dashboardEvents(dashboardSession(),
			&journal.Failure{Signal: machine.ComputationError, Attribution: journal.Attributed, Core: new(0), Offset: new(0), Condition: machine.Alone, Profile: []int{0, 0, 0}},
			&journal.CorePhase{Core: 0, To: journal.PhaseSearch, FailurePoint: new(0)},
			&journal.DeadEnd{Condition: journal.DeadEndFailureAtZero, Core: new(0), Detail: "core 00 failed at CO 0"}), 160, 40},
		{"no-session", nil, 120, 20},
	} {
		t.Run(tc.name, func(t *testing.T) {
			golden(t, "activity-"+tc.name, ansi.Strip(Render(Project(tc.events), tc.width, tc.height, now))+"\n")
		})
	}
}

func TestDashboardNarrowTrackRetainsStages(t *testing.T) {
	t.Parallel()
	goal := Project(dashboardEvents(dashboardSession(),
		&journal.CorePhase{Core: 0, To: journal.PhaseAtLimit, Offset: -50},
		&journal.CorePhase{Core: 1, To: journal.PhaseAtLimit, Offset: -50},
		&journal.CorePhase{Core: 2, To: journal.PhaseAtLimit, Offset: -50},
		&journal.ProfileChange{To: []int{-50, -50, -50}},
		&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: config.Default().Checking.Cycle},
		&journal.CheckingCycle{Cycle: 1, Event: journal.CycleEnd, Passed: true, Full: true},
		&journal.TrialIntent{Trial: "watch", Core: new(0), Offset: new(-50), Regime: machine.R1, Workload: machine.Workloads(machine.R1)[0].ID, DurationS: 120, Condition: machine.Together, Phase: journal.PhaseChecking, Profile: []int{-50, -50, -50}},
		&journal.TrialStart{Trial: "watch"}))
	for _, tc := range []struct {
		name    string
		s       Snapshot
		current string
	}{
		{"search", Project(dashboardEvents(dashboardSession(), &journal.CorePhase{Core: 0, To: journal.PhaseSearch, Offset: -5})), "Find limits"},
		{"checking", Project(dashboardCheckingEvents()), "Test together"},
		{"hunt", Project(dashboardHuntEvents()), "Test together"},
		{"deepening", Project(dashboardDeepeningEvents()), "Go deeper"},
		{"goal", goal, "Keep checking"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			for _, width := range []int{17, 37, 47, 57, 67} {
				lines := track(tc.s.stations(), width)
				for _, line := range lines {
					if cells := ansi.StringWidth(line); cells > width {
						t.Errorf("track line has %d cells, allocated %d: %q", cells, width, ansi.Strip(line))
					}
				}
				frame := ansi.Strip(strings.Join(fit(lines, width, len(lines)+1), "\n"))
				for _, label := range []string{"Find limits", "Test together", "Go deeper", "Clean cycle", "Keep checking"} {
					if !strings.Contains(frame, label) {
						t.Errorf("width %d lost station %q:\n%s", width, label, frame)
					}
				}
				if !strings.Contains(frame, "► "+tc.current) {
					t.Errorf("width %d lost current marker for %q:\n%s", width, tc.current, frame)
				}
				if tc.name != "goal" && !strings.Contains(frame, "○ Clean cycle") {
					t.Errorf("width %d lost goal marker:\n%s", width, frame)
				}
				if tc.name == "goal" && !strings.Contains(frame, "■ Clean cycle") {
					t.Errorf("width %d lost reached goal marker:\n%s", width, frame)
				}
			}
			frame := ansi.Strip(Render(tc.s, 50, 60, time.Unix(1100, 0).UTC()))
			if !strings.Contains(frame, "► "+tc.current) || !strings.Contains(frame, "Clean cycle") || !strings.Contains(frame, "Keep checking") {
				t.Errorf("50-column frame lost stage meaning:\n%s", frame)
			}
		})
	}
}

func TestDashboardShortMainFramesRetainCoreOffsets(t *testing.T) {
	t.Parallel()
	now := time.Unix(1100, 0).UTC()
	for _, tc := range []struct {
		name    string
		events  []journal.Event
		current string
	}{
		{"checking", dashboardCheckingEvents(), "Test together"},
		{"hunt", dashboardHuntEvents(), "Test together"},
		{"deepening", dashboardDeepeningEvents(), "Go deeper"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			s := Project(tc.events)
			for _, width := range []int{50, 90} {
				for _, height := range []int{20, 24} {
					for _, keys := range []bool{false, true} {
						sc := Screen{Width: width, Height: height, Keys: keys}
						drawn := RenderView(s, sc, now)
						frame, scrolled := ansi.Strip(strings.Join(drawn.Lines, "\n")), drawn.Scroll
						if scrolled != 0 {
							t.Errorf("main frame unexpectedly scrolls: %d", scrolled)
						}
						for _, c := range s.cores {
							offset := fmt.Sprintf("%02d  %3d", c.id, c.applied)
							if !strings.Contains(frame, offset) {
								t.Errorf("%dx%d keys=%t hid core offset %q:\n%s", width, height, keys, offset, frame)
							}
						}
						if !strings.Contains(frame, "Clean cycle") || !strings.Contains(frame, "Keep checking") || !strings.Contains(frame, "► "+tc.current) {
							t.Errorf("%dx%d keys=%t lost stage meaning:\n%s", width, height, keys, frame)
						}
						if !strings.Contains(frame, s.story(now).headline) {
							t.Errorf("%dx%d keys=%t lost activity headline:\n%s", width, height, keys, frame)
						}
						if s.trial != nil && !strings.Contains(frame, s.story(now).now.what) {
							t.Errorf("%dx%d keys=%t lost running test:\n%s", width, height, keys, frame)
						}
						lines := drawn.Lines
						if len(lines) != height-1 {
							t.Errorf("frame has %d rows, want %d", len(lines), height-1)
						}
						for _, line := range lines {
							if ansi.StringWidth(line) > width-1 {
								t.Errorf("frame wrote reserved column: %q", line)
							}
						}
					}
				}
			}
		})
	}
	s := Project(dashboardCheckingEvents())
	s.cores = nil
	for id := range 16 {
		s.cores = append(s.cores, coreView{id: id, ccd: id / 8, phase: journal.PhaseAtLimit, applied: -20, tuned: -20})
	}
	frame := strings.Join(RenderView(s, Screen{Width: 90, Height: 24, Keys: true}, now).Lines, "\n")
	if !strings.Contains(ansi.Strip(frame), "00  -20") {
		t.Fatalf("16-core short frame hid all applied offsets:\n%s", ansi.Strip(frame))
	}
}

func TestDashboardCompactStoppedOffsetsKeepDisclaimer(t *testing.T) {
	t.Parallel()
	start := &journal.SessionStart{Session: "stopped"}
	for id := range 16 {
		start.Cores = append(start.Cores, machine.CoreInfo{Core: id, CCD: id / 8, CPUs: []int{2 * id, 2*id + 1}})
	}
	payloads := []journal.Payload{start}
	for id := range 16 {
		payloads = append(payloads,
			&journal.CorePhase{Core: id, To: journal.PhaseAtLimit, Offset: -20, FailurePoint: new(-21)},
			&journal.SMUReadback{Core: id, Offset: 0})
	}
	payloads = append(payloads,
		&journal.ProfileRestored{Offsets: make([]int, 16)},
		&journal.Shutdown{Reason: journal.ShutdownSignal})
	s := Project(dashboardEvents(payloads...))
	if s.stopped == nil || s.cores[0].applied != -20 {
		t.Fatal("fixture must show tuned offsets after restoring hardware to 0")
	}
	for _, width := range []int{20, 50, 90} {
		for _, height := range []int{20, 24} {
			frame := strings.Join(RenderView(s, Screen{Width: width, Height: height, Keys: true}, time.Unix(1100, 0).UTC()).Lines, "\n")
			text := ansi.Strip(frame)
			if !strings.Contains(text, "00  -20") || !strings.Contains(text, "Tuned offsets, not applied now.") && !strings.Contains(text, "Saved, not set") {
				t.Errorf("%dx%d lost tuned-offset meaning:\n%s", width, height, text)
			}
		}
	}
}

func TestDashboardCoreRowsGroupUniqueSortedCCDs(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name string
		ccds []int
		want []string
	}{
		{"interleaved", []int{0, 1, 0, 1}, []string{"CCD 0", "00", "02", "CCD 1", "01", "03"}},
		{"reverse CCD IDs", []int{1, 1, 0, 0}, []string{"CCD 0", "02", "03", "CCD 1", "00", "01"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			session := &journal.SessionStart{Session: "topology"}
			for id, ccd := range tc.ccds {
				session.Cores = append(session.Cores, machine.CoreInfo{Core: id, CCD: ccd, CPUs: []int{2 * id, 2*id + 1}})
			}
			s := Project(dashboardEvents(session))
			var got []string
			for _, line := range s.coreRows(frameWidth) {
				line = strings.TrimSpace(ansi.Strip(line))
				switch {
				case line == "":
				case strings.HasPrefix(line, "CCD "):
					got = append(got, line[:5])
				default:
					got = append(got, line[:2])
				}
			}
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Errorf("CCD/core order (-want +got):\n%s", diff)
			}
		})
	}
}

func TestDashboardViews(t *testing.T) {
	t.Parallel()
	now := time.Unix(1100, 0).UTC()
	s := Project(dashboardCheckingEvents())
	for name, sc := range map[string]Screen{
		"help":     {View: HelpView},
		"help-end": {View: HelpView, Scroll: -1},
		"log":      {View: LogView, Scroll: -1},
	} {
		t.Run(name, func(t *testing.T) {
			sc.Keys, sc.Width, sc.Height = true, 160, 70
			frame := strings.Join(RenderView(s, sc, now).Lines, "\n")
			golden(t, "view-"+name, ansi.Strip(frame)+"\n")
		})
	}
}

func TestDashboardHelpFrameWidth(t *testing.T) {
	t.Parallel()
	now := time.Unix(1100, 0).UTC()
	s := Project(dashboardCheckingEvents())
	for _, width := range []int{80, 160} {
		for _, scroll := range []int{0, -1} {
			t.Run(fmt.Sprintf("width-%d-scroll-%d", width, scroll), func(t *testing.T) {
				frame := strings.Join(RenderView(s, Screen{View: HelpView, Keys: true, Scroll: scroll, Width: width, Height: 70}, now).Lines, "\n")
				limit := margin + min(width-1-margin, frameWidth)
				for i, line := range strings.Split(frame, "\n") {
					if got := ansi.StringWidth(line); got > limit {
						t.Errorf("line %d spans %d cells, frame allows %d: %q", i+1, got, limit, ansi.Strip(line))
					}
				}
			})
		}
	}
}

func TestPress(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name     string
		from     Screen
		key      key
		scrolled int
		want     Screen
		quit     bool
	}{
		{"? opens the help at its top", Screen{Scroll: 3}, "?", 0, Screen{View: HelpView}, false},
		{"? closes the help", Screen{View: HelpView, Scroll: 4}, "?", 9, Screen{}, false},
		{"L opens the log at its end", Screen{}, "L", 0, Screen{View: LogView, Scroll: -1}, false},
		{"l closes the log", Screen{View: LogView}, "l", 0, Screen{}, false},
		{"l from the help opens the log", Screen{View: HelpView, Scroll: 2}, "l", 9, Screen{View: LogView, Scroll: -1}, false},
		{"Esc returns to the main view", Screen{View: LogView, Scroll: 5}, keyEsc, 9, Screen{}, false},
		{"down scrolls the help one line", Screen{View: HelpView}, keyDown, 9, Screen{View: HelpView, Scroll: 1}, false},
		{"j stops at the end", Screen{View: HelpView, Scroll: 9}, "j", 9, Screen{View: HelpView, Scroll: 9}, false},
		{"up from the log's end leaves it", Screen{View: LogView, Scroll: -1}, keyUp, 9, Screen{View: LogView, Scroll: 8}, false},
		{"down to the log's end follows it again", Screen{View: LogView, Scroll: 8}, keyDown, 9, Screen{View: LogView, Scroll: -1}, false},
		{"page down moves a screen less eight lines", Screen{View: HelpView, Height: 30}, keyPageDown, 90, Screen{View: HelpView, Scroll: 22, Height: 30}, false},
		{"page up stops at the top", Screen{View: HelpView, Scroll: 5, Height: 30}, keyPageUp, 90, Screen{View: HelpView, Height: 30}, false},
		{"g goes to the top", Screen{View: LogView, Scroll: -1}, "g", 9, Screen{View: LogView}, false},
		{"End goes to the help's end", Screen{View: HelpView}, keyEnd, 9, Screen{View: HelpView, Scroll: 9}, false},
		{"a scroll beyond the content clamps first", Screen{View: HelpView, Scroll: 50}, keyUp, 9, Screen{View: HelpView, Scroll: 8}, false},
		{"the main view does not scroll", Screen{}, keyDown, 0, Screen{}, false},
		{"other keys change nothing", Screen{View: HelpView, Scroll: 2}, "x", 9, Screen{View: HelpView, Scroll: 2}, false},
		{"q quits", Screen{View: LogView}, "q", 0, Screen{View: LogView}, true},
		{"Ctrl-C quits", Screen{}, "\x03", 0, Screen{}, true},
	} {
		got, quit := press(tc.from, tc.key, tc.scrolled)
		if diff := cmp.Diff(tc.want, got); diff != "" || quit != tc.quit {
			t.Errorf("%s: quit %t, want %t (-want +got):\n%s", tc.name, quit, tc.quit, diff)
		}
	}
}

func TestDecodeKeys(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		in   string
		want []key
	}{
		{"?", []key{"?"}},
		{"\x1b", []key{keyEsc}},
		{"\x1b[A\x1b[B", []key{keyUp, keyDown}},
		{"\x1bOA", []key{keyUp}},
		{"\x1b[5~\x1b[6~", []key{keyPageUp, keyPageDown}},
		{"\x1b[1~\x1b[4~\x1b[H\x1b[F", []key{keyHome, keyEnd, keyHome, keyEnd}},
		{"\x1b[15~q", []key{"q"}},
		{"\x1bq", []key{keyEsc, "q"}},
		{"\x1b[", nil},
	} {
		if diff := cmp.Diff(tc.want, decodeKeys([]byte(tc.in))); diff != "" {
			t.Errorf("%q (-want +got):\n%s", tc.in, diff)
		}
	}
}

func TestScrollbarThumbTracksThePage(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name                       string
		page, total, top, scrolled int
		want                       string
	}{
		{"a page that fits shows no bar", 4, 3, 0, 0, "    "},
		{"the top puts the thumb at the top", 10, 40, 0, 30, "██││││││││"},
		{"the end puts the thumb at the bottom", 10, 40, 30, 30, "││││││││██"},
		{"halfway puts it in the middle", 10, 40, 15, 30, "││││██││││"},
		{"a long view keeps a one-row thumb", 10, 1000, 990, 990, "│││││││││█"},
	} {
		got := ansi.Strip(strings.Join(scrollbar(tc.page, tc.total, tc.top, tc.scrolled), ""))
		if got != tc.want {
			t.Errorf("%s: %q, want %q", tc.name, got, tc.want)
		}
	}
}

func TestRecordOnlyPartialTrial(t *testing.T) {
	t.Parallel()
	profile := []int{-20, -30, -50}
	resumed := []int{-20, -10, -50}
	events := dashboardEvents(dashboardSession(),
		&journal.CorePhase{Core: 0, To: journal.PhaseAtLimit, Offset: -20, FailurePoint: new(-21)},
		&journal.CorePhase{Core: 1, To: journal.PhaseAtLimit, Offset: -30, FailurePoint: new(-31)},
		&journal.CorePhase{Core: 2, To: journal.PhaseAtLimit, Offset: -50},
		&journal.ProfileChange{To: profile},
		&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{machine.R7}},
		&journal.CheckingStep{Cycle: 1, Step: 1, Profile: profile, Partials: []journal.CheckingPartial{{CCD: 0, Cores: []int{1}}, {CCD: 1, Reason: "all CCD cores are at their limits"}}},
		&journal.ProfileChange{From: profile, To: resumed},
		&journal.SMUReadback{Core: 1, Offset: -10},
		&journal.TrialIntent{Trial: "partial", Cores: []int{1}, RecordOnly: true, Condition: machine.Together, Phase: journal.PhaseChecking, Regime: machine.R7, Workload: "mprime-avx2-36k-248k-allcore", Profile: resumed, DurationS: 120, Cycle: 1, Step: 1},
		&journal.TrialStart{Trial: "partial"},
		&journal.TrialEnd{Trial: "partial", Outcome: journal.OutcomeFailure, Signal: machine.ComputationError})
	live := events[:len(events)-1]
	s := Project(live)
	st := s.story(live[len(live)-1].Time.Add(time.Minute))
	if st.now == nil || st.now.what != "cycle 1, step 1 of 1, recorded only" || st.now.detail != "partial all-core load on core 01" {
		t.Fatalf("a running partial must be named record-only with its loaded cores: %+v", st.now)
	}
	if text := strings.Join(st.paragraphs, "\n"); !strings.Contains(text, "moves no offset") || strings.Contains(text, "one core at a time") {
		t.Fatalf("the narrator must say a partial changes nothing and not call it a per-core step:\n%s", text)
	}
	if s.cores[0].loaded || !s.cores[1].loaded || s.cores[1].applied != -10 {
		t.Fatalf("the partial must keep its step-start group after the loaded core becomes shallowest: %+v", s.cores)
	}
	for view, text := range map[string]string{
		"narrator": strings.Join(st.paragraphs, " "),
		"help":     strings.Join(helpLines(frameWidth), " "),
	} {
		text = strings.Join(strings.Fields(ansi.Strip(text)), " ")
		if !strings.Contains(text, "when the step started") || !strings.Contains(text, "even if offsets change") {
			t.Errorf("%s must explain the frozen step-start group, not the current shallowest offsets:\n%s", view, text)
		}
	}
	if !slices.ContainsFunc(s.log, func(l entry) bool {
		return strings.Contains(l.text, "CCD 1 record-only partial skipped: all CCD cores are at their limits")
	}) {
		t.Fatal("the log must show the checking step with its skipped partial's reason")
	}
	ended := Project(events)
	last := ended.history[len(ended.history)-1]
	if got := last.tag + ": " + last.sentence(); got != "fail: partial all-core load on core 01 at -10, wrong result, recorded only" || last.tone != warnTone {
		t.Fatalf("a partial's failure must read as recorded only, not as a decisive failure: %q tone %v", got, last.tone)
	}
}
