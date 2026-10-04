package watch

import (
	"regexp"
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

func TestR7LinesNameTopRequestersAndSelfSufficiency(t *testing.T) {
	t.Parallel()
	workload := machine.Workloads(machine.R7)[0].ID
	s := Snapshot{cycle: &cycleView{}, r7: []tuner.R7CoreStatus{
		{Core: 3, CCD: 0, Workload: workload, TopRequester: true, SelfSufficient: true, Passes: 1},
		{Core: 7, CCD: 0, Workload: workload},
	}}
	for _, p := range []layout{{class: wideLayout, context: rectangle{w: 115}}, {class: compactLayout, context: rectangle{w: 57}}} {
		lines := s.r7Lines(p)
		text := ansi.Strip(strings.Join(lines, "\n"))
		if len(lines) != 2 || !strings.Contains(lines[0], "self-sufficient") {
			t.Fatalf("width %d: want a rule naming the columns and one row per workload:\n%s", p.context.w, text)
		}
		if row := ansi.Strip(lines[1]); !strings.Contains(row, "mprime AVX2") || strings.Count(row, "03") != 2 || strings.Contains(row, "07") {
			t.Fatalf("width %d: row must name core 03 as top requester and self-sufficient, never core 07: %q", p.context.w, row)
		}
		if (p.class == wideLayout) != strings.Contains(text, "not a guarantee") {
			t.Fatalf("width %d: only a wide panel has room to say the evidence is no guarantee:\n%s", p.context.w, text)
		}
	}
	s.r7[0].OffsetFallback = true
	if row := ansi.Strip(s.r7Lines(layout{context: rectangle{w: 57}})[1]); !strings.Contains(row, "03 by offset") {
		t.Fatalf("a fallback must not claim a voltage measurement: %q", row)
	}
	// Offset proxies stay labelled even when the list of cores does not fit.
	many := Snapshot{cycle: &cycleView{}}
	for id := range 16 {
		many.r7 = append(many.r7, tuner.R7CoreStatus{Core: id, CCD: id / 8, Workload: workload, TopRequester: id%2 == 0, OffsetFallback: true})
	}
	for _, p := range []layout{{class: wideLayout, context: rectangle{w: 115}}, {class: mediumLayout, context: rectangle{w: 80}}, {class: compactLayout, context: rectangle{w: 57}}} {
		if row := ansi.Strip(many.r7Lines(p)[1]); !strings.Contains(row, "... by offset") {
			t.Fatalf("width %d: the cut top requesters lost their offset label: %q", p.context.w, row)
		}
	}
	// A measured CCD's top requesters never take the other CCD's offset label when the cell is cut.
	mixed := Snapshot{cycle: &cycleView{}}
	for id := range 16 {
		mixed.r7 = append(mixed.r7, tuner.R7CoreStatus{Core: id, CCD: id / 8, Workload: workload, TopRequester: id%2 == 0 && id < 8 || id == 8, OffsetFallback: id >= 8})
	}
	for _, p := range []layout{{class: wideLayout, context: rectangle{w: 115}}, {class: mediumLayout, context: rectangle{w: 80}}, {class: compactLayout, context: rectangle{w: 57}}} {
		row := ansi.Strip(mixed.r7Lines(p)[1])
		if !strings.Contains(row, "; 08 by offset") || strings.Contains(row, "06 by offset") {
			t.Fatalf("width %d: measured and offset top requesters lost their own labels: %q", p.context.w, row)
		}
	}
	// Too narrow for both lists, the dropped measured cores still leave a cut marker.
	if got := topCell([]int{0}, []int{8, 9, 10, 11, 12, 13, 14, 15}, 17); got != "...;... by offset" {
		t.Fatalf("narrow mixed cell = %q", got)
	}
	// The tuning boot's console font covers IBM437 only, so truncation uses ASCII.
	narrow := ansi.Strip(s.r7Lines(layout{context: rectangle{w: 24}})[1])
	if !strings.HasSuffix(narrow, "...") || strings.ContainsFunc(narrow, func(r rune) bool { return r > 0x7e }) {
		t.Fatalf("narrow row %q must truncate with ASCII", narrow)
	}
	if lines := (Snapshot{r7: s.r7}).r7Lines(layout{context: rectangle{w: 57}}); lines != nil {
		t.Fatalf("R7 status appeared before checking had a cycle: %q", lines)
	}
}

func TestR7EvidenceStaysOutOfTheForecast(t *testing.T) {
	t.Parallel()
	w := machine.Workloads(machine.R7)[0].ID
	s := Snapshot{cycle: &cycleView{}, r7: []tuner.R7CoreStatus{{Core: 0, Workload: w, TopRequester: true, SelfSufficient: true}, {Core: 1, Workload: w}}}
	if rows := s.outcomeRows(); len(rows) != 0 {
		t.Fatalf("evidence must not invent forecast branches: %+v", rows)
	}
}

func TestShortDashboardReachesEveryR7Workload(t *testing.T) {
	t.Parallel()
	for _, c := range watchCuts(t) {
		if c.name != "checking" {
			continue
		}
		s, now := Project(c.events), cutTime(c.events)
		populated := s
		populated.r7 = slices.Clone(s.r7)
		for i := range populated.r7 {
			populated.r7[i].SelfSufficient = populated.r7[i].Core%2 == 0
		}
		for _, snapshot := range []Snapshot{s, populated} {
			for _, keys := range []bool{false, true} {
				frame := ansi.Strip(strings.Join(RenderView(snapshot, Screen{View: MainView, Width: 120, Height: 33, Keys: keys}, now).Lines, "\n"))
				if !strings.Contains(frame, "self-sufficient") {
					t.Errorf("120x33 frame (keys %t) omits the R7 status:\n%s", keys, frame)
				}
				for _, w := range machine.Workloads(machine.R7) {
					name := strings.Join(strings.Fields(workloadLabel(w.ID))[:2], " ")
					if !strings.Contains(frame, "\n "+name+" ") {
						t.Errorf("120x33 frame (keys %t) omits the R7 %s status:\n%s", keys, w.ID, frame)
					}
				}
				if !strings.Contains(frame, "trial 1 of") {
					t.Errorf("120x33 frame (keys %t) pushed the running part out of the cycle checklist:\n%s", keys, frame)
				}
				if !regexp.MustCompile(`\n \+\d+ more +.*\n +.*\n R7 ─`).MatchString(frame) {
					t.Errorf("120x33 frame (keys %t) cuts the cycle checklist without saying how much it leaves out:\n%s", keys, frame)
				}
				// Two rows shorter, the overflow count gives way to the running part.
				short := ansi.Strip(strings.Join(RenderView(snapshot, Screen{View: MainView, Width: 120, Height: 31, Keys: keys}, now).Lines, "\n"))
				if !strings.Contains(short, "trial 1 of") {
					t.Errorf("120x31 frame (keys %t) pushed the running part out of the cycle checklist:\n%s", keys, short)
				}
			}
		}
	}
}

func TestCoreRowsMarkTheRunningWorkloadsTopRequesters(t *testing.T) {
	t.Parallel()
	w := machine.Workloads(machine.R7)[1].ID
	s := Snapshot{
		cores: []coreView{{id: 0, ccd: 0, state: coreAtLimit}, {id: 1, ccd: 0, state: coreAtLimit}},
		trial: &trialView{regime: machine.R7, workload: machine.Workload{ID: w}},
		r7: []tuner.R7CoreStatus{
			{Core: 0, Workload: machine.Workloads(machine.R7)[0].ID, TopRequester: true},
			{Core: 1, Workload: w, TopRequester: true, OffsetFallback: true},
		},
	}
	for _, tc := range []struct {
		width int
		want  string
	}{{75, "top by offset"}, {57, "top"}} {
		p := measure(s, Screen{Width: 2*tc.width + 6, Height: 45})
		p.ccds[0].w = tc.width
		c := newCanvas(p)
		drawCCD(&c, p.ccds[0], p.columns[0], 0, s)
		rows := c.lines()[p.ccds[0].y+1 : p.ccds[0].y+3]
		if strings.Contains(ansi.Strip(rows[0]), "top") || !strings.HasSuffix(strings.TrimRight(ansi.Strip(rows[1]), " "), tc.want) {
			t.Fatalf("width %d: only core 01, top requester of the running workload, is marked %q:\n%s", tc.width, tc.want, ansi.Strip(strings.Join(rows, "\n")))
		}
	}
}

func TestR7BackoffForecastIsConditionalOnEarlierRequests(t *testing.T) {
	t.Parallel()
	w := machine.Workloads(machine.R7)[0].ID
	backoff := &journal.TunerDecision{Core: 0, Decision: journal.Backoff, FromOffset: -31, ToOffset: -30}
	next := &tuner.Trial{Regime: machine.R7, Workload: w, Cores: []int{0, 1}, DurationS: 120, Cycle: 1}
	for _, tc := range []struct {
		fallback bool
		want     string
	}{{false, "core 00 -31 → -30 per earlier requests"}, {true, "core 00 -31 → -30 per offset order"}} {
		// Full-part status says the opposite: the forecast's own loaded set decides its basis.
		s := Snapshot{
			trial:    &trialView{regime: machine.R7, cores: []int{0, 1}, cycle: 1, duration: 2 * time.Minute, workload: machine.Workload{ID: w}, index: 1, of: 4},
			r7:       []tuner.R7CoreStatus{{Core: 0, Workload: w, TopRequester: true, OffsetFallback: !tc.fallback}, {Core: 1, Workload: w, OffsetFallback: !tc.fallback}},
			outcomes: []outcome{{premise: ifPasses, next: next, withoutTelemetry: true}, {premise: ifUnnamed, decisions: []journal.Payload{backoff}, next: next, withoutTelemetry: true, offsetOrder: tc.fallback}},
		}
		rows := s.outcomeRows()
		if len(rows) != 2 || strings.Contains(fitPhrases(rows[0].phrases, 200), "per ") || rows[0].basis != "" {
			t.Fatalf("a pass decides nothing from requests: %+v", rows)
		}
		if got := fitPhrases(rows[1].phrases, 200); !strings.HasPrefix(got, tc.want) || rows[1].basis == "" {
			t.Fatalf("backoff without telemetry = %q, want it to start %q", got, tc.want)
		}
		p := measure(s, Screen{Width: 120, Height: 33})
		c := newCanvas(p)
		drawOutcomes(&c, p, s)
		if line := ansi.Strip(c.lines()[p.outcomes.y]); !strings.Contains(line, "backoffs "+rows[1].basis) {
			t.Fatalf("compact outcome line dropped the basis: %q", line)
		}
	}
}

func TestOutcomeRowsKeepEveryR7NamedOutcome(t *testing.T) {
	t.Parallel()
	w := machine.Workloads(machine.R7)[0].ID
	loaded := []int{0, 1, 2, 3}
	rerun := &tuner.Trial{Regime: machine.R7, Workload: w, Cores: loaded, DurationS: 120, Rerun: true}
	same := &tuner.Trial{Regime: machine.R7, Workload: w, Cores: loaded, DurationS: 120, Cycle: 1, Step: 1}
	backoff := func(core int) *journal.TunerDecision {
		return &journal.TunerDecision{Core: core, Decision: journal.Backoff, FromOffset: -30, ToOffset: -16, FailurePoint: new(-30)}
	}
	s := Snapshot{
		cores: []coreView{{id: 0}, {id: 1}, {id: 2}, {id: 3}},
		trial: &trialView{regime: machine.R7, workload: machine.Workload{ID: w}, cores: loaded, cycle: 1, step: 1, duration: 2 * time.Minute, index: 1, of: 4},
		outcomes: []outcome{
			{premise: ifPasses, next: same, withoutTelemetry: true},
			{premise: ifNamed, core: new(0), decisions: []journal.Payload{&journal.Failure{Core: new(0)}, backoff(0)}, next: rerun, withoutTelemetry: true},
			{premise: ifNamed, core: new(2), atZero: true, decisions: []journal.Payload{&journal.Failure{Core: new(2)}, backoff(1)}, next: rerun, withoutTelemetry: true},
			{premise: ifNamed, core: new(3), atZero: true, top: true, decisions: []journal.Payload{&journal.Failure{Core: new(3)}, &journal.DeadEnd{Condition: journal.DeadEndFailureAtZero}}, withoutTelemetry: true},
			{premise: ifUnnamed, decisions: []journal.Payload{&journal.Failure{}, backoff(1)}, next: rerun, withoutTelemetry: true},
			{premise: ifInconclusive, next: same},
		},
	}
	var got []string
	for _, row := range s.outcomeRows() {
		got = append(got, row.label+" | "+fitPhrases(row.phrases, 400))
	}
	want := []string{
		"if it passes | next: trial 2 of 4",
		"if another core is named | its failure point is recorded → it backs off per earlier requests → next: rerun R7 all-core on 00-03 · 2m · a top requester at 0: tuning stops",
		"if none or a core at 0 is named | core 01 -30 → -16 per earlier requests → next: rerun R7 all-core on 00-03 · 2m",
	}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("outcome rows (-want +got):\n%s", diff)
	}
}

func TestZeroOffsetCoresOnDifferentCCDsAreNamedApart(t *testing.T) {
	t.Parallel()
	s := Snapshot{
		cores: []coreView{{id: 0, ccd: 0}, {id: 1, ccd: 0}, {id: 2, ccd: 1}, {id: 3, ccd: 1}},
		outcomes: []outcome{
			{premise: ifNamed, core: new(1), atZero: true},
			{premise: ifNamed, core: new(2), atZero: true},
		},
	}
	if got := []string{s.zeroWords(s.outcomes[0]), s.zeroWords(s.outcomes[1])}; !slices.Equal(got, []string{"a CCD 0 core at 0", "a CCD 1 core at 0"}) {
		t.Fatalf("zero-offset cores whose failures move different CCDs = %q", got)
	}
}

func TestPassForecastNamesTheDerivedPartial(t *testing.T) {
	t.Parallel()
	w := machine.Workloads(machine.R7)[0].ID
	full := []int{0, 1, 2, 3}
	s := Snapshot{
		trial: &trialView{regime: machine.R7, workload: machine.Workload{ID: w}, cores: full, cycle: 1, step: 1, part: 1, parts: 2, duration: 2 * time.Minute, index: 4, of: 4, passed: 3},
		cycle: &cycleView{number: 1, open: true, steps: []cycleStep{{regime: machine.R7, workload: machine.Workload{ID: w}, more: true, parts: []cyclePart{{cores: full, ccd: 0, full: true, short: 4, running: true}, {cores: []int{0, 1, 2, 3, 4, 5, 6, 7}, ccd: -1, short: 4}}}}},
	}
	partial := &tuner.Trial{Regime: machine.R7, Workload: w, Cores: []int{0, 1, 2}, DurationS: 120, Cycle: 1, Step: 1}
	for _, tc := range []struct {
		sources []int
		basis   string
	}{{[]int{5}, "per earlier requests"}, {nil, "per offset order"}} {
		chain := &journal.CheckingChain{Cycle: 1, Step: 1, CCD: 0, Workload: w, Cores: []int{0, 1, 2}, SourceSeqs: tc.sources}
		phrases, _, basis := s.outcomeWords(outcome{premise: ifPasses, passes: 1, decisions: []journal.Payload{chain}, next: partial, withoutTelemetry: true, offsetOrder: tc.sources == nil})
		text := fitPhrases(phrases, 400)
		if basis != tc.basis || !strings.Contains(text, "CCD 0 partial on 00-02 "+tc.basis) || !strings.Contains(text, "next: step 1: R7 all-core on 00-02 with") {
			t.Fatalf("pass forecast %q (basis %q) hides the derived partial or its %q basis", text, basis, tc.basis)
		}
	}
}

func TestNamedZeroCoreMovingItsCCDRecordsNoOwnFailurePoint(t *testing.T) {
	t.Parallel()
	moved := &journal.TunerDecision{Core: 1, Decision: journal.Backoff, FromOffset: -30, ToOffset: -16, FailurePoint: new(-30)}
	phrases := (Snapshot{}).decisionPhrases([]journal.Payload{&journal.Failure{Core: new(2)}, moved}, true)
	if got := fitPhrases(phrases, 400); got != "core 01 -30 → -16" {
		t.Fatalf("named core 02 at 0 moved core 01: %q", got)
	}
	own := &journal.TunerDecision{Core: 2, Decision: journal.Backoff, FromOffset: -30, ToOffset: -29, FailurePoint: new(-30)}
	if got := fitPhrases((Snapshot{}).decisionPhrases([]journal.Payload{&journal.Failure{Core: new(2)}, own}, true), 400); got != "its failure point is recorded → it backs off" {
		t.Fatalf("named core 02 moved itself: %q", got)
	}
}

func TestTopNoteFitsBesideANoteAtItsExactWidth(t *testing.T) {
	t.Parallel()
	note := "held by C4 · solo -31"
	width := ansi.StringWidth(note) + ansi.StringWidth(" · ") + len("top requester")
	if got := ansi.Strip(topNote(note, false, width)); got != note+" · top requester" {
		t.Fatalf("exact-fit note = %q", got)
	}
	if got := ansi.Strip(topNote(note, false, width-1)); got != note {
		t.Fatalf("one cell short = %q", got)
	}
}
