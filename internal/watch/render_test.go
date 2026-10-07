package watch

import (
	"fmt"
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

func TestHelpWideUsesThreeColumns(t *testing.T) {
	t.Parallel()
	body, scroll := renderHelpBody(235, 61, 0)
	if scroll != 0 {
		t.Fatalf("wide help unexpectedly needs scrolling: %d", scroll)
	}
	if len(body) == 0 {
		t.Fatal("empty help")
	}
	first := ansi.Strip(body[0])
	for _, title := range []string{"READING THE SCREEN", "WHAT TOGI DOES", "WORDS"} {
		if !strings.Contains(first, title) {
			t.Errorf("wide help lost column %q: %q", title, first)
		}
	}
	text := ansi.Strip(strings.Join(body, "\n"))
	for _, title := range []string{"THE TOP LINE", "KINDS OF LOAD", "NO ETA", "CLEAN CYCLES"} {
		if !strings.Contains(text, title) {
			t.Errorf("wide help lost %q", title)
		}
	}
	for row, line := range body {
		if ansi.StringWidth(line) > 235 {
			t.Errorf("row %d exceeds help rectangle", row)
		}
	}
}

func TestHelpNarrowScrollsOneColumn(t *testing.T) {
	t.Parallel()
	top, scroll := renderHelpBody(77, 19, 0)
	end, endScroll := renderHelpBody(77, 19, -1)
	clamped, _ := renderHelpBody(77, 19, scroll+100)
	if scroll == 0 || endScroll != scroll {
		t.Fatalf("narrow help must scroll: top=%d end=%d", scroll, endScroll)
	}
	if diff := cmp.Diff(end, clamped); diff != "" {
		t.Errorf("scroll beyond end (-want +got):\n%s", diff)
	}
	if !strings.Contains(ansi.Strip(top[0]), "READING THE SCREEN") {
		t.Fatalf("help does not open at the first section: %q", top[0])
	}
	if text := ansi.Strip(strings.Join(end, "\n")); !strings.Contains(text, "KINDS OF LOAD") || !strings.Contains(text, "R7 all-core") {
		t.Fatalf("help end loses the last section: %s", text)
	}
	if strings.Contains(ansi.Strip(top[0]), "WHAT TOGI DOES") {
		t.Fatal("narrow help squeezes columns together")
	}
	for _, lines := range [][]string{top, end} {
		if len(lines) > 19 {
			t.Errorf("help exceeds its row allocation: %d", len(lines))
		}
		for _, line := range lines {
			if ansi.StringWidth(line) > 77 {
				t.Errorf("help exceeds its column allocation: %q", line)
			}
		}
	}
}

func TestLogFollowsNewEntriesOnlyAtEnd(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 1, 1, 12, 0, 0, 0, time.UTC)
	s := Snapshot{session: true, start: now}
	for i := range 30 {
		s.log = append(s.log, entry{at: now.Add(time.Duration(i) * time.Second), tag: "trial.end", text: fmt.Sprintf("entry %02d", i), tone: goodTone})
	}
	top, topScroll := renderLogBody(s, 77, 10, 0)
	end, endScroll := renderLogBody(s, 77, 10, -1)
	if diff := cmp.Diff(topScroll, endScroll); diff != "" {
		t.Errorf("scroll extent (-want +got):\n%s", diff)
	}
	last := ansi.Strip(end[len(end)-1])
	for _, want := range []string{"12:00:29", "trial.end", "entry 29"} {
		if !strings.Contains(last, want) {
			t.Errorf("last journal entry loses %q: %s", want, last)
		}
	}
	s.log = append(s.log, entry{at: now.Add(30 * time.Second), tag: "crash.detected", text: "new crash", tone: badTone})
	nextTop, _ := renderLogBody(s, 77, 10, 0)
	nextEnd, nextScroll := renderLogBody(s, 77, 10, -1)
	for row := range top {
		want, got := ansi.Strip(top[row]), ansi.Strip(nextTop[row])
		if len([]rune(want)) > 2 && len([]rune(got)) > 2 {
			want, got = string([]rune(want)[2:]), string([]rune(got)[2:])
		}
		if diff := cmp.Diff(want, got); diff != "" {
			t.Errorf("pinned journal page row %d (-want +got):\n%s", row, diff)
		}
	}
	if diff := cmp.Diff(endScroll+1, nextScroll); diff != "" {
		t.Errorf("append grows scroll extent (-want +got):\n%s", diff)
	}
	if last := ansi.Strip(nextEnd[len(nextEnd)-1]); !strings.Contains(last, "12:00:30") || !strings.Contains(last, "crash.detected") || !strings.Contains(last, "new crash") {
		t.Fatalf("following the end loses appended entry: %s", last)
	}
}

func TestHelpLogBodiesFitSmallRectangles(t *testing.T) {
	t.Parallel()
	s := Snapshot{log: []entry{{at: time.Unix(0, 0).UTC(), tag: "trial.intent", text: strings.Repeat("long journal line ", 20)}}}
	for _, width := range []int{0, 1, 2, 10, 77, 155, 235} {
		for _, height := range []int{0, 1, 19, 61} {
			for _, scroll := range []int{0, 7, -1} {
				help, _ := renderHelpBody(width, height, scroll)
				log, _ := renderLogBody(s, width, height, scroll)
				for _, body := range [][]string{help, log} {
					if len(body) > height {
						t.Errorf("%dx%d: body has %d rows", width, height, len(body))
					}
					for _, line := range body {
						if ansi.StringWidth(line) > width {
							t.Errorf("%dx%d: body writes beyond rectangle: %q", width, height, line)
						}
					}
				}
			}
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

func TestMediumCoreNotesRetainCombinationAndSoloOffsets(t *testing.T) {
	t.Parallel()
	now := time.Unix(1000, 0).UTC()
	s := Snapshot{session: true, start: now, cores: []coreView{
		{id: 0, ccd: 0, state: coreAtLimit, profile: -26, applied: -26, solo: new(-31), holder: &limit{combination: 4}},
		{id: 8, ccd: 1, state: coreHasRoom, profile: -36, applied: -36, solo: new(-37), gaveBack: 1},
	}}
	frame := ansi.Strip(Render(s, 160, 45, now))
	for _, note := range []string{"C4 · solo -31", "C1 · solo -37"} {
		if !strings.Contains(frame, note) {
			t.Errorf("medium frame hid constraint facts %q:\n%s", note, frame)
		}
	}
}

func TestOutcomeLinesMergeBranchesAndNameTheSamePartsTrials(t *testing.T) {
	t.Parallel()
	next := &tuner.Trial{Regime: machine.R7, Cores: []int{1, 2}, Cycle: 1, Step: 1, DurationS: 120, Workload: "w"}
	long := *next
	long.DurationS = 300
	s := Snapshot{
		trial: &trialView{regime: machine.R7, cores: []int{1, 2}, cycle: 1, step: 1, recordOnly: true, duration: 2 * time.Minute, workload: machine.Workload{ID: "w"}, index: 1, of: 4},
		outcomes: []outcome{
			{premise: ifPasses, next: next},
			{premise: ifAllPass, passes: 3, next: &long},
			{premise: ifNamed, next: next},
			{premise: ifUnnamed, next: next},
		},
	}
	var got []string
	for _, row := range s.outcomeRows() {
		got = append(got, row.label+": "+fitPhrases(row.phrases, 200))
	}
	want := []string{
		"if 3 of 4 pass: recorded only, moves nothing → next: trial 4 of 4 · 5m",
		"pass or fail: recorded only, moves nothing → next: trial 2 of 4",
	}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("outcome rows (-want +got):\n%s", diff)
	}
}

func TestPassingOutcomeNamesTheNextPartOrStep(t *testing.T) {
	t.Parallel()
	cycle := &cycleView{number: 1, open: true, steps: []cycleStep{
		{regime: machine.R2, parts: []cyclePart{{cores: []int{1}, ccd: -1}, {cores: []int{9}, ccd: -1}}},
		{regime: machine.R6, parts: []cyclePart{{cores: []int{1, 9}, ccd: -1}}},
	}}
	perCore := func(core, part int) *trialView {
		return &trialView{regime: machine.R2, cores: []int{core}, core: core, condition: machine.Alone, duration: 2 * time.Minute, cycle: 1, step: 1, part: part, parts: 2, index: 1, of: 1}
	}
	for _, tc := range []struct {
		name  string
		trial *trialView
		next  tuner.Trial
		want  string
	}{
		{"the next core of a per-core step", perCore(1, 1), tuner.Trial{Regime: machine.R2, Core: 9, Offset: -16, Condition: machine.Together, Cycle: 1, Step: 1, DurationS: 120},
			"if it passes: part 1 is done → next: part 2: core 09 at -16, 2m"},
		{"the next step", perCore(9, 2), tuner.Trial{Regime: machine.R6, Cores: []int{1, 9}, Condition: machine.Together, Cycle: 1, Step: 2, DurationS: 900, Workload: "mprime-sse-4k-21k-idle"},
			"if it passes: step 1 is done → next: step 2: R6 idle + bursts with mprime SSE 4K-21K"},
	} {
		s := Snapshot{cycle: cycle, trial: tc.trial, outcomes: []outcome{{premise: ifPasses, passes: 1, next: &tc.next}}}
		rows := s.outcomeRows()
		if len(rows) != 1 {
			t.Fatalf("%s: rows=%+v", tc.name, rows)
		}
		if got := rows[0].label + ": " + fitPhrases(rows[0].phrases, 200); got != tc.want {
			t.Errorf("%s:\n got %q\nwant %q", tc.name, got, tc.want)
		}
	}
}

func TestNarratorWrapsUnderItsText(t *testing.T) {
	t.Parallel()
	st := story{label: "HUNT 6", lines: []string{strings.Repeat("word ", 15)}}
	lines := narratorLines(st, 60, 2, false)
	if len(lines) != 2 {
		t.Fatalf("want two lines, got %q", lines)
	}
	first, second := ansi.Strip(lines[0]), ansi.Strip(lines[1])
	if text := strings.Index(first, "word"); text != strings.Index(second, "word") || text <= strings.Index(first, "HUNT 6") {
		t.Fatalf("second line must start under the text, not the label:\n%s\n%s", first, second)
	}
}

func TestRestingBandLeavesItsRowsToThePanels(t *testing.T) {
	t.Parallel()
	now := time.Unix(1000, 0).UTC()
	s := Snapshot{session: true, start: now, stopped: &stopView{at: now, reason: journal.ShutdownSignal}, cores: []coreView{{id: 0}}}
	for _, tc := range []struct {
		width, height, gap int
	}{{240, 67, 3}, {160, 45, 3}, {120, 33, 2}} {
		lines := strings.Split(ansi.Strip(Render(s, tc.width, tc.height, now)), "\n")
		band := slices.IndexFunc(lines, func(l string) bool { return strings.HasPrefix(strings.TrimSpace(l), "STOPPED") })
		ccd := slices.IndexFunc(lines, func(l string) bool { return strings.Contains(l, "CCD 0") })
		if band < 0 || ccd-band != tc.gap {
			t.Errorf("%dx%d: a one-line band at row %d must leave the CCD table at row %d, not %d", tc.width, tc.height, band, band+tc.gap, ccd)
		}
	}
}

func TestUndrawableGlyphsAreMeasuredEscaped(t *testing.T) {
	t.Parallel()
	text := "journal error: " + strings.Repeat("界", 60)
	if got := ansi.Strip(strings.Join(wrapStyled(text, 100, textStyle), "")); strings.Count(got, `\u754c`) != 60 {
		t.Fatalf("wrapped escapes lost text: %q", got)
	}
	if got := trimWords(text, 30); ansi.StringWidth(got) > 30 || !strings.HasPrefix(got, "journal error") {
		t.Fatalf("trimmed escape %q", got)
	}
}

func TestCCDRoleCountsLoadAndProbes(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name  string
		cores []coreView
		want  string
	}{
		{"parked with a load left", []coreView{{ccd: 0, state: coreParked, loaded: true}, {ccd: 0, state: coreParked}}, "parked at 0, 1 under load"},
		{"parked and idle", []coreView{{ccd: 0, state: coreParked}, {ccd: 0, state: coreParked}}, "parked at 0, idle"},
		{"a member probed", []coreView{{ccd: 0, state: coreMember, loaded: true}, {ccd: 0, state: coreProbe, loaded: true}}, "members, one probed shallower than its failing offset"},
	} {
		if got, _ := (Snapshot{cores: tc.cores}).ccdRole(0); got != tc.want {
			t.Errorf("%s: %q, want %q", tc.name, got, tc.want)
		}
	}
}

func TestRestoredDeadEndLabelsSavedRows(t *testing.T) {
	t.Parallel()
	s := Project(dashboardEvents(dashboardSession(),
		&journal.CorePhase{Core: 0, To: journal.PhaseHasRoom, Offset: -25},
		&journal.DeadEnd{Condition: journal.DeadEndNoEvidence, Detail: "five trials in a row proved nothing"},
		&journal.ProfileRestored{Offsets: []int{0, 0, 0}},
		&journal.Shutdown{Reason: journal.ShutdownDeadEnd}))
	if s.deadEnd == nil || s.stopped == nil || !s.stopped.saved {
		t.Fatalf("fixture must end in a restored dead end: %+v %+v", s.deadEnd, s.stopped)
	}
	for _, size := range [][2]int{{240, 67}, {160, 45}, {120, 33}} {
		if text := ansi.Strip(Render(s, size[0], size[1], time.Unix(2000, 0).UTC())); !strings.Contains(text, "Rows show the saved profile, not applied now.") {
			t.Errorf("%dx%d restored dead end presents saved offsets as applied:\n%s", size[0], size[1], text)
		}
	}
}

func TestHuntStageStaysOnAnUnfinishedPartBetweenTrials(t *testing.T) {
	t.Parallel()
	h := &huntView{groups: []groupView{{id: 4, outcome: "running"}}, plan: []huntPart{{group: 4, outcome: "running"}, {}}}
	s := Snapshot{hunt: h}
	if got := s.huntStage(true); got != "part 1 of 2" {
		t.Fatalf("between trials of an unfinished part 1: %q", got)
	}
	h.groups = append(h.groups, groupView{id: 5, outcome: "running"})
	h.plan[0].outcome = "pass"
	h.plan[1] = huntPart{group: 5, outcome: "running", running: true}
	if got := s.huntStage(true); got != "part 2 of 2" {
		t.Fatalf("after part 1 passed: %q", got)
	}
}

func TestProbeWithoutFailingOffsetRenders(t *testing.T) {
	t.Parallel()
	s := Snapshot{hunt: &huntView{probes: []probeView{{member: 0, now: -9, running: true}}}}
	if lines := s.probeLines(tables{}, 120, mediumLayout); len(lines) == 0 {
		t.Fatal("a running probe without a recorded failing offset drew nothing")
	}
}

func TestPartialTrialUsesOrdinaryEvidence(t *testing.T) {
	t.Parallel()
	profile := []int{-20, -30, -50}
	resumed := []int{-20, -10, -50}
	events := dashboardEvents(dashboardSession(),
		&journal.CorePhase{Core: 0, To: journal.PhaseAtLimit, Offset: -20, FailurePoint: new(-21)},
		&journal.CorePhase{Core: 1, To: journal.PhaseAtLimit, Offset: -30, FailurePoint: new(-31)},
		&journal.CorePhase{Core: 2, To: journal.PhaseAtLimit, Offset: -50},
		&journal.ProfileChange{To: profile},
		&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{machine.R7}},
		&journal.CheckingChain{Cycle: 1, Step: 1, CCD: 0, Workload: "mprime-avx2-36k-248k-allcore", Profile: profile, Groups: [][]int{{0}, {1}}, Cores: []int{1}, Part: "partial 1"},
		&journal.ProfileChange{From: profile, To: resumed},
		&journal.SMUReadback{Core: 1, Offset: -10},
		&journal.TrialIntent{Trial: "partial", Cores: []int{1}, Condition: machine.Together, Phase: journal.PhaseChecking, Regime: machine.R7, Workload: "mprime-avx2-36k-248k-allcore", Profile: resumed, DurationS: 120, Cycle: 1, Step: 1},
		&journal.TrialStart{Trial: "partial"},
		&journal.TrialEnd{Trial: "partial", Outcome: journal.OutcomeFailure, Signal: machine.ComputationError})
	live := events[:len(events)-1]
	s := Project(live)
	if s.trial == nil || !s.trial.partial || s.trial.recordOnly {
		t.Fatalf("partial must be ordinary evidence: %+v", s.trial)
	}
	if s.cores[0].loaded || !s.cores[1].loaded || s.cores[1].applied != -10 {
		t.Fatalf("partial must preserve its loaded set after offsets change: %+v", s.cores)
	}
	if got := ansi.Strip(s.operation(*s.trial)); !strings.Contains(got, "PARTIAL") || strings.Contains(got, "RECORD ONLY") {
		t.Fatalf("NOW must identify ordinary partial evidence: %q", got)
	}
	if !slices.ContainsFunc(s.log, func(l entry) bool {
		return strings.Contains(l.text, "next partial 1 loads cores 01")
	}) {
		t.Fatal("log must show the derived chain part")
	}
	ended := Project(events)
	last := ended.history[0]
	if got := last.tag + ": " + last.sentence(); !strings.Contains(got, "partial CCD 0") || !strings.Contains(got, "wrong result") || strings.Contains(got, "record only") || last.tone != badTone {
		t.Fatalf("partial failure must be decisive: %q tone %v", got, last.tone)
	}
}

func TestAllZeroRerunIsNamedForWhatItDecides(t *testing.T) {
	t.Parallel()
	profile := []int{-20, 0, -10}
	idle := machine.Workloads(machine.R6)[0].ID
	events := dashboardEvents(dashboardSession(),
		&journal.ProfileChange{To: profile},
		&journal.TrialIntent{Trial: "failed", Cores: []int{0, 1, 2}, Condition: machine.Together, Phase: journal.PhaseChecking, Regime: machine.R6, Workload: idle, Profile: profile, DurationS: 120},
		&journal.TrialStart{Trial: "failed"},
		&journal.TrialEnd{Trial: "failed", Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(1)},
		&journal.Failure{Trial: "failed", Signal: machine.ComputationError, Attribution: journal.Attributed, Core: new(1), Offset: new(0), Condition: machine.Together, Regime: machine.R6, Profile: profile},
		&journal.TrialIntent{Trial: "zero", Cores: []int{0, 1, 2}, Condition: machine.Parked, Phase: journal.PhaseChecking, Regime: machine.R6, Workload: idle, Profile: []int{0, 0, 0}, DurationS: 120, Rerun: true},
		&journal.TrialStart{Trial: "zero"},
		&journal.TrialEnd{Trial: "zero", Outcome: journal.OutcomePass})
	s := Project(events[:len(events)-1])
	if s.trial == nil || !s.trial.zeroRerun() {
		t.Fatalf("trial in flight %+v", s.trial)
	}
	if got := s.story(time.Time{}).label; got != "RERUN AT CO 0" {
		t.Fatalf("story label %q", got)
	}
	if got := ansi.Strip(s.operation(*s.trial)); got != "RERUN AT CO 0" {
		t.Fatalf("NOW %q", got)
	}
	ended := Project(events)
	if got := ended.history[0].sentence(); !strings.Contains(got, "rerun of R6 idle + bursts on 00-02 with every core at 0") {
		t.Fatalf("history %q", got)
	}
}

func TestForecastNamesTheAllZeroRerunAfterARerun(t *testing.T) {
	t.Parallel()
	idle := machine.Workloads(machine.R6)[0].ID
	loaded := []int{0, 1}
	again := &tuner.Trial{Regime: machine.R6, Workload: idle, Cores: loaded, Condition: machine.Together, Profile: []int{0, -10}, DurationS: 120, Rerun: true, Retry: true}
	zero := &tuner.Trial{Regime: machine.R6, Workload: idle, Cores: loaded, Condition: machine.Parked, Profile: []int{0, 0}, DurationS: 120, Rerun: true}
	s := Snapshot{
		cores: []coreView{{id: 0}, {id: 1}},
		trial: &trialView{regime: machine.R6, workload: machine.Workload{ID: idle}, cores: loaded, condition: machine.Together, profile: []int{0, -10}, duration: 2 * time.Minute, rerun: true},
		outcomes: []outcome{
			{premise: ifInconclusive, next: again},
			{premise: ifNamed, core: new(0), atZero: true, decisions: []journal.Payload{&journal.Failure{Core: new(0)}}, next: zero, withoutTelemetry: true},
		},
	}
	var got []string
	for _, row := range s.outcomeRows() {
		got = append(got, row.label+" | "+fitPhrases(row.phrases, 400))
	}
	if len(got) != 2 || !slices.ContainsFunc(got, func(l string) bool { return strings.HasSuffix(l, "the same trial runs again") }) {
		t.Fatalf("a retry of the rerun is the same trial: %q", got)
	}
	if !slices.ContainsFunc(got, func(l string) bool {
		return strings.HasSuffix(l, "next: rerun R6 idle + bursts on 00 01 with every core at 0 · 2m")
	}) {
		t.Fatalf("a core at 0 named on a rerun must forecast the all-zero rerun: %q", got)
	}
}
