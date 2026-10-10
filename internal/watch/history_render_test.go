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
)

func TestHeldJournalArrivalNoticeFitsFrame(t *testing.T) {
	t.Parallel()
	now := time.Unix(1000, 0).UTC()
	for _, size := range [][2]int{{50, 33}, {80, 33}, {120, 33}, {160, 45}, {240, 67}} {
		t.Run(fmt.Sprintf("%dx%d", size[0], size[1]), func(t *testing.T) {
			t.Parallel()
			s := Project(warningEvents(logLimit + 50))
			sc := Screen{View: LogView, Scroll: 0, Width: size[0], Height: size[1], Keys: true}
			var held heldLog
			frame := RenderView(held.apply(s, sc), sc, now)
			assertFrameBounds(t, frame, sc)
			p := measure(s, sc)
			wantBody := frame.Lines[p.body.y : p.body.y+p.body.h]
			if got := ansi.Strip(frame.Lines[p.hint]); strings.Contains(got, "new event") {
				t.Fatalf("new hold has an arrival notice: %q", got)
			}

			for _, arrivals := range []int{1, 123} {
				s = Project(warningEvents(logLimit + 50 + arrivals))
				updated := RenderView(held.apply(s, sc), sc, now)
				assertFrameBounds(t, updated, sc)
				if diff := cmp.Diff(wantBody, updated.Lines[p.body.y:p.body.y+p.body.h]); diff != "" {
					t.Errorf("%d arrivals move the held list (-want +got):\n%s", arrivals, diff)
				}
				if diff := cmp.Diff(frame.Scroll, updated.Scroll); diff != "" {
					t.Errorf("%d arrivals move the held scroll (-want +got):\n%s", arrivals, diff)
				}
				count := fmt.Sprintf("%d new events", arrivals)
				if arrivals == 1 {
					count = "1 new event"
				}
				want := count + " · End: latest"
				if got := ansi.Strip(updated.Lines[p.hint]); !strings.Contains(got, want) {
					t.Errorf("arrival count and return action are not visible: got %q, want %q", got, want)
				}
			}
		})
	}
}

func TestHistoryPanelTinyAndTallBudgetsCountDroppedEntries(t *testing.T) {
	t.Parallel()
	s := Project(warningEvents(700))
	if diff := cmp.Diff(701, s.historyDropped+len(s.history)); diff != "" {
		t.Fatalf("fixture history total (-want +got):\n%s", diff)
	}
	for _, tc := range []struct {
		height, rows, shown int
		footer              string
	}{
		{0, 0, 0, ""},
		{1, 1, 0, "+701 more"},
		{2, 2, 0, "+701 more"},
		{3, 3, 0, "+701 more"},
		{10, 10, 7, "+694 more"},
		{202, 202, 199, "+502 more"},
		{203, 203, 200, "+501 more"},
		{222, 203, 200, "+501 more"},
	} {
		t.Run(fmt.Sprint(tc.height), func(t *testing.T) {
			t.Parallel()
			rows := s.historyPanel(100, tc.height)
			assertHistoryPanelBudget(t, rows, 100, tc.height, tc.rows, tc.footer)
			if len(rows) != tc.rows {
				return
			}
			for i, e := range s.history[:tc.shown] {
				before, alarm, after := e.sentenceParts()
				want := cutWords(before+alarm+after, 84)
				if got := ansi.Strip(rows[2+i]); !strings.HasSuffix(got, want) {
					t.Errorf("visible entry %d is not the newest retained entry: got %q, want suffix %q", i, got, want)
				}
			}
		})
	}
	if rows := s.historyPanel(0, 10); len(rows) != 0 {
		t.Errorf("zero-width panel returns rows: %q", rows)
	}
}

func TestHistoryPanelEmptyAndSmallBudgets(t *testing.T) {
	t.Parallel()
	type budget struct {
		height, rows int
		footer       string
	}
	for _, tc := range []struct {
		name    string
		s       Snapshot
		budgets []budget
	}{
		{"empty", Snapshot{}, []budget{{0, 0, ""}, {1, 1, ""}, {2, 2, ""}, {3, 2, ""}, {10, 2, ""}}},
		{"one entry", Project(warningEvents(0)), []budget{{0, 0, ""}, {1, 1, "+1 more"}, {2, 2, "+1 more"}, {3, 3, ""}, {10, 3, ""}}},
		{"three entries", Project(warningEvents(2)), []budget{{0, 0, ""}, {1, 1, "+3 more"}, {2, 2, "+3 more"}, {3, 3, "+3 more"}, {4, 4, "+2 more"}, {5, 5, ""}, {10, 5, ""}}},
		{"only dropped entries", Snapshot{historyDropped: 7}, []budget{{0, 0, ""}, {1, 1, "+7 more"}, {2, 2, "+7 more"}, {3, 3, "+7 more"}, {10, 3, "+7 more"}}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			for _, b := range tc.budgets {
				t.Run(fmt.Sprint(b.height), func(t *testing.T) {
					rows := tc.s.historyPanel(100, b.height)
					assertHistoryPanelBudget(t, rows, 100, b.height, b.rows, b.footer)
				})
			}
		})
	}
}

func assertHistoryPanelBudget(t *testing.T, rows []string, width, height, wantRows int, footer string) {
	t.Helper()
	if len(rows) > height {
		t.Errorf("panel returns %d rows for a %d-row rectangle", len(rows), height)
	}
	if diff := cmp.Diff(wantRows, len(rows)); diff != "" {
		t.Errorf("panel row count (-want +got):\n%s", diff)
	}
	for i, row := range rows {
		if cells := ansi.StringWidth(row); cells > width {
			t.Errorf("row %d exceeds %d cells (%d): %q", i, width, cells, ansi.Strip(row))
		}
		got := ansi.Strip(row)
		if strings.HasPrefix(got, "+") && (i != len(rows)-1 || got != footer) {
			t.Errorf("unexpected hidden-entry count at row %d: %q, want final row %q", i, got, footer)
		}
	}
	if footer != "" && len(rows) > 0 {
		if diff := cmp.Diff(footer, ansi.Strip(rows[len(rows)-1])); diff != "" {
			t.Errorf("hidden-entry footer (-want +got):\n%s", diff)
		}
	}
}

func TestRenderedTinyHistoryCountsAllHiddenEntries(t *testing.T) {
	t.Parallel()
	s := Project(warningEvents(700))
	s.trial = &trialView{}
	s.cores = nil
	for id := range 16 {
		s.cores = append(s.cores, coreView{id: id, ccd: id / 8})
	}
	now := time.Unix(1000, 0).UTC()
	for _, width := range []int{120, 160, 240} {
		for _, height := range []int{24, 25} {
			t.Run(fmt.Sprintf("%dx%d", width, height), func(t *testing.T) {
				t.Parallel()
				sc := Screen{Width: width, Height: height, Keys: true}
				p := measure(s, sc)
				if diff := cmp.Diff(height-23, p.history.h); diff != "" {
					t.Fatalf("fixture history rectangle height (-want +got):\n%s", diff)
				}
				drawn := RenderView(s, sc, now)
				assertFrameBounds(t, drawn, sc)
				row := drawn.Lines[p.history.y+p.history.h-1]
				got := strings.TrimSpace(ansi.Strip(ansi.Cut(row, p.history.x, p.history.x+p.history.w)))
				if diff := cmp.Diff("+701 more", got); diff != "" {
					t.Errorf("visible history footer (-want +got):\n%s", diff)
				}
			})
		}
	}
}

func TestRenderedCombinationOverflowNamesOmittedCoresAndBoundedLog(t *testing.T) {
	t.Parallel()
	now := time.Unix(1000, 0).UTC()
	for _, tc := range []struct {
		ids     []int
		omitted string
	}{
		{[]int{0, 1, 2, 3, 4, 5, 6, 7, 9}, "09"},
		{[]int{0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 15}, "09 11 15"},
	} {
		t.Run(fmt.Sprintf("%d members", len(tc.ids)), func(t *testing.T) {
			t.Parallel()
			s := Snapshot{session: true, start: now, combos: []comboView{{id: 1, hunt: 7, probed: true}}}
			for id := range 16 {
				s.cores = append(s.cores, coreView{id: id, ccd: id / 8, profile: -20})
			}
			for _, id := range slices.Backward(tc.ids) {
				s.combos[0].members = append(s.combos[0].members, journal.CombinationMember{Core: id, Offset: -25})
			}
			sc := Screen{Width: 240, Height: 67, Keys: true}
			p := measure(s, sc)
			if p.class != wideLayout || p.tables.member != 5 || p.tables.combos[1].w != 40 {
				t.Fatalf("fixture does not use the wide table's eight member columns: %+v", p.tables)
			}
			drawn := RenderView(s, sc, now)
			assertFrameBounds(t, drawn, sc)
			var context []string
			for _, row := range drawn.Lines[p.context.y : p.context.y+p.context.h] {
				context = append(context, strings.TrimSpace(ansi.Strip(ansi.Cut(row, p.context.x, p.context.x+p.context.w))))
			}
			want := fmt.Sprintf("columns show 8 of %d member cores; not shown: %s · l: last 400 events", len(tc.ids), tc.omitted)
			if diff := cmp.Diff(want, context[3]); diff != "" {
				t.Errorf("visible omission and journal-bound label (-want +got):\n%s", diff)
			}
			for _, id := range tc.ids[:8] {
				if !strings.Contains(context[2], fmt.Sprintf("%02d", id)) {
					t.Errorf("visible member header omits core %02d: %q", id, context[2])
				}
			}
			for _, id := range tc.ids[8:] {
				if strings.Contains(context[2], fmt.Sprintf("%02d", id)) {
					t.Errorf("omitted core %02d is still in the member header: %q", id, context[2])
				}
			}
		})
	}
}
