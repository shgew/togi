package watch

import (
	"fmt"
	"os"
	"path/filepath"
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

func journalViewData(t *testing.T, msg string, warnings int) []byte {
	t.Helper()
	data := watchSessionLine(t, msg)
	for i := 1; i < warnings; i++ {
		data = append(data, warningLine(t, i+2, fmt.Sprintf("%s %d", msg, i))...)
	}
	return data
}

func writeJournalViewFile(t *testing.T, path string, data []byte) {
	t.Helper()
	if err := os.WriteFile(path, data, 0644); err != nil {
		t.Fatal(err)
	}
	stamp := time.Unix(2000, 0)
	if err := os.Chtimes(path, stamp, stamp); err != nil {
		t.Fatal(err)
	}
}

func scrolledJournalSource(t *testing.T, data []byte) (*source, Screen) {
	t.Helper()
	dir := t.TempDir()
	writeJournalViewFile(t, filepath.Join(dir, "events.jsonl"), data)
	src := &source{dir: dir}
	if !src.reload() || src.snap.Err() != nil {
		t.Fatalf("load initial journal: %v", src.snap.Err())
	}
	following := Screen{View: LogView, Scroll: -1, Width: 120, Height: 33, Keys: true}
	drawn := src.frame(following)
	if drawn.Scroll == 0 {
		t.Fatal("fixture journal does not need scrolling")
	}
	back, _ := press(following, keyHome, drawn.Scroll)
	src.frame(back)
	return src, back
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

func TestScrolledBackJournalViewRecoversUnavailableJournal(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name        string
		unavailable string
		redraw      bool
	}{
		{"missing before redraw", "missing", false},
		{"missing after redraw", "missing", true},
		{"malformed before redraw", "malformed", false},
		{"malformed after redraw", "malformed", true},
		{"empty journal", "empty", true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			data := journalViewData(t, "original", 40)
			src, back := scrolledJournalSource(t, data)
			path := filepath.Join(src.dir, "events.jsonl")
			data = append(data, warningLine(t, 42, "old arrival")...)
			writeJournalViewFile(t, path, data)
			if !src.reload() || !strings.Contains(hint(src.frame(back)), "1 new event ") {
				t.Fatal("fixture did not establish a held journal with an arrival")
			}
			original := watchFileInfo(t, path)
			switch tc.unavailable {
			case "missing":
				if err := os.Remove(path); err != nil {
					t.Fatal(err)
				}
			case "malformed":
				writeJournalViewFile(t, path, append(data, []byte("not json\n")...))
			case "empty":
				writeJournalViewFile(t, path, nil)
			}
			if !src.reload() {
				t.Fatal("unavailable journal was not reloaded")
			}
			if got := src.snap.Err() != nil; got != (tc.unavailable == "malformed") {
				t.Fatalf("unexpected unavailable journal error: %v", src.snap.Err())
			}
			if tc.redraw {
				for range 2 {
					drawn := src.frame(back)
					rows := strings.Join(plainRows(drawn), "\n")
					if !strings.Contains(rows, "No journal entries yet.") || strings.Contains(rows, "original") {
						t.Fatalf("unavailable journal retained old rows:\n%s", rows)
					}
					if got := hint(drawn); strings.Contains(got, "new event") {
						t.Fatalf("unavailable journal retained arrivals: %q", got)
					}
				}
			}
			if src.reload() {
				t.Fatal("unchanged unavailable journal was repeatedly reloaded")
			}

			// Keep the eligible count at the old journal's count, and grow the malformed file on recovery.
			writeJournalViewFile(t, path, journalViewData(t, "recovered", 41))
			recovered := watchFileInfo(t, path)
			if tc.unavailable == "malformed" && (!os.SameFile(original, recovered) || recovered.Size() <= original.Size()+int64(len("not json\n"))) {
				t.Fatal("malformed recovery did not preserve the inode and grow the file")
			}
			if !src.reload() || src.snap.Err() != nil {
				t.Fatalf("load recovered journal: %v", src.snap.Err())
			}
			drawn := src.frame(back)
			rows := strings.Join(plainRows(drawn), "\n")
			if !strings.Contains(rows, "recovered") || strings.Contains(rows, "original") || strings.Contains(rows, "No journal entries yet.") {
				t.Fatalf("recovered journal did not replace the unavailable held list:\n%s", rows)
			}
			if got := hint(drawn); strings.Contains(got, "new event") {
				t.Fatalf("recovered journal inherited arrivals: %q", got)
			}
		})
	}
}

func TestScrolledBackJournalViewDiscardsChangedJournal(t *testing.T) {
	t.Parallel()
	for _, operation := range []string{"replace", "shrink"} {
		for _, warnings := range []int{41, 42} {
			t.Run(fmt.Sprintf("%s/%d warnings", operation, warnings), func(t *testing.T) {
				t.Parallel()
				msg := "original"
				if operation == "shrink" {
					msg += " " + strings.Repeat("padding ", 40)
				}
				data := journalViewData(t, msg, 40)
				src, back := scrolledJournalSource(t, data)
				path := filepath.Join(src.dir, "events.jsonl")
				writeJournalViewFile(t, path, append(data, warningLine(t, 42, "old arrival")...))
				if !src.reload() || !strings.Contains(hint(src.frame(back)), "1 new event ") {
					t.Fatal("fixture did not establish a held journal with an arrival")
				}
				original := watchFileInfo(t, path)
				target := path
				if operation == "replace" {
					target = filepath.Join(src.dir, "replacement")
				}
				writeJournalViewFile(t, target, journalViewData(t, "replaced", warnings))
				if operation == "replace" {
					if err := os.Rename(target, path); err != nil {
						t.Fatal(err)
					}
				}
				changed := watchFileInfo(t, path)
				if !original.ModTime().Equal(changed.ModTime()) {
					t.Fatal("fixture did not preserve modification time")
				}
				switch operation {
				case "replace":
					if os.SameFile(original, changed) {
						t.Fatal("replacement did not change the inode")
					}
					if warnings == 41 && original.Size() != changed.Size() {
						t.Fatal("equal-count replacement did not isolate the changed inode")
					}
				case "shrink":
					if !os.SameFile(original, changed) || changed.Size() >= original.Size() {
						t.Fatal("shrink did not preserve the inode and reduce the size")
					}
				}
				if !src.reload() || src.snap.Err() != nil {
					t.Fatalf("load changed journal: %v", src.snap.Err())
				}
				if diff := cmp.Diff(warnings+1, src.snap.logTotal); diff != "" {
					t.Fatalf("changed journal's eligible count (-want +got):\n%s", diff)
				}
				drawn := src.frame(back)
				rows := strings.Join(plainRows(drawn), "\n")
				if !strings.Contains(rows, "replaced") || strings.Contains(rows, "original") || strings.Contains(rows, "old arrival") {
					t.Fatalf("changed journal retained previous rows:\n%s", rows)
				}
				if got := hint(drawn); strings.Contains(got, "new event") {
					t.Fatalf("changed journal inherited arrivals: %q", got)
				}
			})
		}
	}
}

func TestScrolledBackJournalViewHoldsSourceAppendsAndSessionStart(t *testing.T) {
	t.Parallel()
	src, back := scrolledJournalSource(t, journalViewData(t, "original", 40))
	want := plainRows(src.frame(back))
	path := filepath.Join(src.dir, "events.jsonl")
	original := watchFileInfo(t, path)
	j, err := journal.Open(src.dir, journal.Options{Boot: "boot", Now: func() time.Time { return time.Unix(1100, 0).UTC() }})
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := j.Close(); err != nil {
			t.Error(err)
		}
	}()
	nextSession := *j.Events()[0].Data.(*journal.SessionStart)
	nextSession.Session = "next"
	for i, payload := range []journal.Payload{
		&journal.SessionWarning{Operation: "retain", Error: "ordinary arrival"},
		&nextSession,
		&journal.SessionWarning{Operation: "retain", Error: "latest arrival"},
	} {
		if _, err := j.Append(payload); err != nil {
			t.Fatal(err)
		}
		appended := watchFileInfo(t, path)
		if !os.SameFile(original, appended) || appended.Size() <= original.Size() {
			t.Fatal("append did not preserve the inode and grow the file")
		}
		original = appended
		if !src.reload() || src.snap.Err() != nil {
			t.Fatalf("load appended journal: %v", src.snap.Err())
		}
		drawn := src.frame(back)
		if diff := cmp.Diff(want, plainRows(drawn)); diff != "" {
			t.Errorf("%s moved held rows (-want +got):\n%s", payload.Kind(), diff)
		}
		if got := hint(drawn); !strings.Contains(got, plural(i+1, "new event")+" ") {
			t.Errorf("%s did not count %d arrivals: %q", payload.Kind(), i+1, got)
		}
		if i == 1 && j.Events()[len(j.Events())-1].Kind != journal.KindSessionStart {
			t.Fatal("new-session append included another event")
		}
	}
	end, _ := press(back, keyEnd, src.frame(back).Scroll)
	latest := src.frame(end)
	if last := lastEntry(latest); !strings.Contains(last, "latest arrival") {
		t.Errorf("End does not show the newest source event: %q", last)
	}
	if got := hint(latest); strings.Contains(got, "new event") {
		t.Errorf("End retained the source arrivals: %q", got)
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
