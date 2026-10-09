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

// warningEvents is a session start followed by n distinct session warnings, "warning 0" to "warning n-1".
func warningEvents(n int) []journal.Event {
	events := dashboardEvents(dashboardSession())
	for i := range n {
		e := dashboardEvents(&journal.SessionWarning{Operation: "write state projection", Error: fmt.Sprint(i)})[0]
		e.Seq = len(events) + 1
		e.Time = events[0].Time.Add(time.Duration(e.Seq) * time.Second)
		e.Msg = fmt.Sprintf("warning %d", i)
		events = append(events, e)
	}
	return events
}

// plainRows strips styling and drops the header, which shows the clock, and the row that lists the keys, which says
// how many events arrived.
func plainRows(d Drawn) []string {
	var rows []string
	for _, line := range d.Lines[1:] {
		if row := ansi.Strip(line); !strings.Contains(row, "close view") {
			rows = append(rows, row)
		}
	}
	return rows
}

func hint(d Drawn) string {
	for _, line := range d.Lines {
		if row := ansi.Strip(line); strings.Contains(row, "close view") {
			return row
		}
	}
	return ""
}

// lastEntry is the lowest row of the journal view that holds text.
func lastEntry(d Drawn) string {
	for _, row := range slices.Backward(plainRows(d)) {
		if strings.TrimSpace(row) != "" {
			return row
		}
	}
	return ""
}

func TestScrolledBackJournalViewHoldsItsListAndCountsArrivals(t *testing.T) {
	t.Parallel()
	// A full window: every new event pushes the oldest one out, which used to slide the rows under the reader.
	src := &source{snap: Project(warningEvents(logLimit + 50))}
	following := Screen{View: LogView, Scroll: -1, Width: 120, Height: 33, Keys: true}
	drawn := src.frame(following)
	if got := hint(drawn); strings.Contains(got, "new event") {
		t.Fatalf("a view at the end counts arrivals: %q", got)
	}

	back, _ := press(following, keyHome, drawn.Scroll)
	held := src.frame(back)
	want := plainRows(held)

	src.snap = Project(warningEvents(logLimit + 51))
	one := src.frame(back)
	if diff := cmp.Diff(want, plainRows(one)); diff != "" {
		t.Errorf("one event arrived while scrolled back; the rows moved (-want +got):\n%s", diff)
	}
	if got := hint(one); !strings.Contains(got, "1 new event ") {
		t.Errorf("one arrival not counted: %q", got)
	}

	src.snap = Project(warningEvents(logLimit + 58))
	many := src.frame(back)
	if diff := cmp.Diff(want, plainRows(many)); diff != "" {
		t.Errorf("seven more events arrived while scrolled back; the rows moved (-want +got):\n%s", diff)
	}
	if got := hint(many); !strings.Contains(got, "8 new events") {
		t.Errorf("arrivals not counted: %q", got)
	}
	if diff := cmp.Diff(held.Scroll, many.Scroll); diff != "" {
		t.Errorf("scroll extent moved with the arrivals (-want +got):\n%s", diff)
	}

	end, _ := press(back, keyEnd, many.Scroll)
	latest := src.frame(end)
	if last := lastEntry(latest); !strings.Contains(last, fmt.Sprintf("warning %d", logLimit+57)) {
		t.Errorf("End does not show the newest event: %q", last)
	}
	if got := hint(latest); strings.Contains(got, "new event") {
		t.Errorf("a view back at the end still counts arrivals: %q", got)
	}

	// Scrolling back again holds the list as it is now, not as it was the first time.
	again := src.frame(back)
	if got := hint(again); strings.Contains(got, "new event") {
		t.Errorf("a new hold starts with arrivals: %q", got)
	}
}

func TestClosingScrolledBackJournalViewLetsGoOfItsList(t *testing.T) {
	t.Parallel()
	src := &source{snap: Project(warningEvents(logLimit + 10))}
	following := Screen{View: LogView, Scroll: -1, Width: 120, Height: 33, Keys: true}
	back, _ := press(following, keyHome, src.frame(following).Scroll)
	src.frame(back)
	src.snap = Project(warningEvents(logLimit + 20))
	src.frame(Screen{View: MainView, Width: 120, Height: 33, Keys: true})
	reopened := src.frame(following)
	if last := lastEntry(reopened); !strings.Contains(last, fmt.Sprintf("warning %d", logLimit+19)) {
		t.Errorf("reopened journal view lacks the newest event: %q", last)
	}
}

func TestProjectCountsEveryListedJournalEvent(t *testing.T) {
	t.Parallel()
	events := warningEvents(logLimit + 30)
	listed := len(events)
	for i := range 90 {
		for _, p := range []journal.Payload{&journal.SMUIntent{Op: journal.SMUSet, Core: new(0), Offset: -20}, &journal.PreflightCheck{}} {
			e := dashboardEvents(p)[0]
			e.Seq = len(events) + 1 + i
			events = append(events, e)
		}
	}
	s := Project(events)
	if diff := cmp.Diff([]int{listed, logLimit}, []int{s.logTotal, len(s.log)}); diff != "" {
		t.Errorf("the journal view counts what it lists, SMU and preflight events left out (-want +got):\n%s", diff)
	}
}

func TestHistoryPanelCountsEveryEntryItLeavesOut(t *testing.T) {
	t.Parallel()
	const entries = 2*historyLimit + 6 // the session start and 2*historyLimit+5 warnings
	s := Project(warningEvents(entries - 1))
	if diff := cmp.Diff(entries, s.historyDropped+len(s.history)); diff != "" {
		t.Fatalf("kept and dropped entries add up to every entry (-want +got):\n%s", diff)
	}
	for _, tc := range []struct {
		name   string
		height int
		rows   int
		more   string
	}{
		{"a frame taller than the kept entries", 2 + historyLimit + 20, 2 + historyLimit + 1, fmt.Sprintf("+%d more", entries-historyLimit)},
		{"a frame too short for the kept entries", 2 + 50, 2 + 50, fmt.Sprintf("+%d more", entries-49)},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			rows := s.historyPanel(100, tc.height)
			if len(rows) != tc.rows {
				t.Fatalf("got %d rows, want %d", len(rows), tc.rows)
			}
			if diff := cmp.Diff(tc.more, ansi.Strip(rows[len(rows)-1])); diff != "" {
				t.Errorf("last row (-want +got):\n%s", diff)
			}
		})
	}

	short := Project(warningEvents(4))
	rows := short.historyPanel(100, 40)
	if len(rows) != 2+5 || strings.Contains(ansi.Strip(rows[len(rows)-1]), "more") {
		t.Errorf("a journal that fits says something is left out: %q", rows)
	}
}
