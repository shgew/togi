package watch

import (
	"testing"

	"github.com/charmbracelet/x/ansi"
	"github.com/google/go-cmp/cmp"
)

func TestCutWordsKeepsWholeWordsAndMarksTheCut(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name, text string
		width      int
		want       string
	}{
		{"fits", "idle until noon", 15, "idle until noon"},
		{"cut at a word", "so a freeze needs a manual reset", 22, "so a freeze needs a..."},
		{"never inside a word", "heavy vector load", 13, "heavy..."},
		{"separator before the marker goes", "31 failures · 8 crashes", 17, "31 failures..."},
		{"one word that does not fit is omitted", "top", 2, ""},
		{"a long word that does not fit is only the marker", "requester", 8, "..."},
		{"no room", "top", 0, ""},
		{"width counts cells, not bytes", "██ ██ ██", 7, "██..."},
		{"marker glyph counts as its console form", "a … b", 5, "a..."},
	} {
		got := ansi.Strip(cutWords(tc.text, tc.width))
		if diff := cmp.Diff(tc.want, got); diff != "" {
			t.Errorf("%s: cutWords(%q, %d) mismatch (-want +got):\n%s", tc.name, tc.text, tc.width, diff)
		}
		if w := ansi.StringWidth(got); w > tc.width {
			t.Errorf("%s: %q is %d cells in %d", tc.name, got, w, tc.width)
		}
	}
}

func TestFitClausesLeavesOutLowerPriorityClausesBeforeCutting(t *testing.T) {
	t.Parallel()
	clauses := []clause{
		whole("", "returns to -37"),
		{" · ", []form{{"solo -40", 1}, {}}},
		{" · ", []form{{"held by C2", 3}, {"C2", 2}, {}}},
	}
	for _, tc := range []struct {
		width int
		want  string
	}{
		{40, "returns to -37 · solo -40 · held by C2"},
		{37, "returns to -37 · solo -40 · C2"},
		{29, "returns to -37 · solo -40"},
		{24, "returns to -37"},
		{14, "returns to -37"},
		{13, "returns to..."},
	} {
		got := fitClauses(tc.width, clauses...)
		if diff := cmp.Diff(tc.want, got); diff != "" {
			t.Errorf("width %d (-want +got):\n%s", tc.width, diff)
		}
	}
}

func TestNoteLineKeepsItsValueAtEveryWidth(t *testing.T) {
	t.Parallel()
	back, solo := -37, -38
	returns := coreView{returnsTo: &back}
	for _, tc := range []struct {
		core  coreView
		width int
		want  string
	}{
		{returns, 20, "returns to -37"},
		{returns, 14, "returns to -37"},
		{returns, 13, " -37"},
		{returns, 12, " -37"},
		{returns, 4, " -37"},
		{returns, 3, "-37"},
		{coreView{state: coreFound, solo: &solo}, 13, "solo -38"},
		{coreView{state: coreFound, solo: &solo}, 7, "-38"},
		{coreView{gaveBack: 3, solo: &solo}, 17, "C3 · solo -38"},
		{coreView{gaveBack: 3, solo: &solo}, 6, "C3"},
	} {
		got := ansi.Strip(tc.core.noteLine(tc.width))
		if diff := cmp.Diff(tc.want, got); diff != "" {
			t.Errorf("noteLine(%d) (-want +got):\n%s", tc.width, diff)
		}
	}
}

func TestTopNoteIsAWordWholeOrNothing(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		note  string
		width int
		want  string
	}{
		{"", 2, ""},
		{"", 3, "top"},
		{"", 13, "top requester"},
		{"-11", 5, "-11"},
		{"-11", 9, "-11 · top"},
	} {
		got := ansi.Strip(topNote(tc.note, false, tc.width))
		if diff := cmp.Diff(tc.want, got); diff != "" {
			t.Errorf("topNote(%q, %d) (-want +got):\n%s", tc.note, tc.width, diff)
		}
	}
}
