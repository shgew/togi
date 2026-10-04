package watch

import (
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/charmbracelet/x/ansi"

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
		s := Snapshot{
			trial:    &trialView{regime: machine.R7, cores: []int{0, 1}, cycle: 1, duration: 2 * time.Minute, workload: machine.Workload{ID: w}, index: 1, of: 4},
			r7:       []tuner.R7CoreStatus{{Core: 0, Workload: w, TopRequester: true, OffsetFallback: tc.fallback}, {Core: 1, Workload: w, OffsetFallback: tc.fallback}},
			outcomes: []outcome{{premise: ifPasses, next: next, withoutTelemetry: true}, {premise: ifUnnamed, decisions: []journal.Payload{backoff}, next: next, withoutTelemetry: true}},
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
