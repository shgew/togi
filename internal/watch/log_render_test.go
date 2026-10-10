package watch

import (
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/charmbracelet/x/ansi"
	"github.com/google/go-cmp/cmp"
)

// logBodyText is the text the journal view draws in body row row: the cells right of the two-cell scrollbar area, with
// trailing blanks dropped. It also returns how many text cells the journal view has.
func logBodyText(t *testing.T, s Snapshot, sc Screen, row int) (string, int) {
	t.Helper()
	frame := RenderView(s, sc, time.Unix(2000, 0).UTC())
	assertFrameBounds(t, frame, sc)
	p := measure(s, sc)
	if row >= p.body.h {
		t.Fatalf("%dx%d: body has %d rows, want row %d", sc.Width, sc.Height, p.body.h, row)
	}
	line := frame.Lines[p.body.y+row]
	return strings.TrimRight(ansi.Strip(ansi.Cut(line, p.body.x+2, p.body.x+p.body.w)), " "), p.body.w - 2
}

const startMessage = "session dashboard started by a togi build from before version stamps (schema 0, ruleset 0, fixes 0, 3 cores)"

func TestLogBodyRowKeepsWholeValuesAtEveryWidth(t *testing.T) {
	t.Parallel()
	s := Project(warningEvents(0))
	if diff := cmp.Diff([]entry{{at: time.Unix(1000, 0).UTC(), tag: tagStart, text: startMessage}}, s.log, cmp.AllowUnexported(entry{})); diff != "" {
		t.Fatalf("fixture journal (-want +got):\n%s", diff)
	}
	stamp := wallSecond(s.log[0].at)
	pad := strings.Repeat(" ", 20-len(tagStart))
	for _, tc := range []struct {
		width, text int
		want        string
	}{
		{10, 5, ""},
		{13, 8, stamp},
		{16, 11, stamp},
		{19, 14, stamp},
		{20, 15, stamp + "  start"},
		{31, 26, stamp + "  start"},
		{32, 27, stamp + "  start"},
		{37, 32, stamp + "  start"},
		{41, 36, stamp + "  start"},
		{46, 41, stamp + "  start"},
		{47, 42, stamp + "  start" + pad + "  session..."},
		{57, 52, stamp + "  start" + pad + "  session dashboard..."},
		{63, 58, stamp + "  start" + pad + "  session dashboard..."},
		{160, 155, stamp + "  start" + pad + "  " + startMessage},
	} {
		t.Run(fmt.Sprint(tc.width), func(t *testing.T) {
			t.Parallel()
			sc := Screen{View: LogView, Scroll: 0, Width: tc.width, Height: 33, Keys: true}
			got, text := logBodyText(t, s, sc, 2)
			if diff := cmp.Diff(tc.text, text); diff != "" {
				t.Fatalf("journal text cells (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Errorf("journal row (-want +got):\n%s", diff)
			}
		})
	}
	// The row is the time, then the event, then the message's leading words in order. cutWords drops the separators
	// that end the last word it keeps before its marker, so "0," may show as "0...".
	words := strings.Fields(startMessage)
	for width := 6; width <= 160; width++ {
		sc := Screen{View: LogView, Scroll: 0, Width: width, Height: 33, Keys: true}
		got, text := logBodyText(t, s, sc, 2)
		if ansi.StringWidth(got) > text {
			t.Errorf("width %d: journal row exceeds %d text cells: %q", width, text, got)
		}
		fields := strings.Fields(got)
		if len(fields) > 0 && fields[0] != stamp {
			t.Errorf("width %d: journal row cuts its time: %q", width, got)
		}
		if len(fields) > 1 && fields[1] != tagStart {
			t.Errorf("width %d: journal row cuts its event: %q", width, got)
		}
		message := fields[min(len(fields), 2):]
		if len(message) > len(words) {
			t.Errorf("width %d: journal message has extra words: %q", width, got)
			continue
		}
		for i, field := range message {
			want := words[i]
			if last := i == len(message)-1; last && strings.HasSuffix(field, cutMarker) {
				field, want = strings.TrimRight(strings.TrimSuffix(field, cutMarker), ",;:"), strings.TrimRight(want, ",;:")
			} else if last && len(message) < len(words) {
				t.Errorf("width %d: journal message ends early without %q: %q", width, cutMarker, got)
			}
			if field != want {
				t.Errorf("width %d: journal message word %d is %q, want %q: %q", width, i, field, want, got)
			}
		}
	}
}

func TestLogBodyEmptyNoticeKeepsWholeWords(t *testing.T) {
	t.Parallel()
	s := Project(nil)
	if len(s.log) != 0 {
		t.Fatalf("fixture journal is not empty: %v", s.log)
	}
	for _, tc := range []struct {
		width, text int
		want        string
	}{
		{8, 3, ""},
		{10, 5, "No..."},
		{20, 15, "No journal..."},
		{27, 22, "No journal entries..."},
		{28, 23, "No journal entries yet."},
		{120, 115, "No journal entries yet."},
	} {
		t.Run(fmt.Sprint(tc.width), func(t *testing.T) {
			t.Parallel()
			sc := Screen{View: LogView, Scroll: 0, Width: tc.width, Height: 33, Keys: true}
			got, text := logBodyText(t, s, sc, 2)
			if diff := cmp.Diff(tc.text, text); diff != "" {
				t.Fatalf("journal text cells (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Errorf("empty journal notice (-want +got):\n%s", diff)
			}
		})
	}
}

func TestLogBodyWideRowsAlignTagsInLocalTime(t *testing.T) {
	t.Parallel()
	at := time.Date(2026, 1, 1, 23, 59, 58, 0, time.UTC)
	s := Snapshot{session: true, start: at, log: []entry{
		{at: at, tag: tagStart, text: "first message", tone: goodTone},
		{at: at.Add(time.Second), tag: "crash.detected", text: "second message", tone: badTone},
	}}
	body, _ := renderLogBody(s, 120, 10, 0)
	for i, want := range []string{
		at.Local().Format("15:04:05") + "  start" + strings.Repeat(" ", 15) + "  first message",
		at.Add(time.Second).Local().Format("15:04:05") + "  crash.detected" + strings.Repeat(" ", 6) + "  second message",
	} {
		if got := strings.TrimRight(ansi.Strip(body[2+i]), " "); got != "  "+want {
			t.Errorf("wide journal row %d (-want +got):\n-%q\n+%q", i, "  "+want, got)
		}
	}
}
