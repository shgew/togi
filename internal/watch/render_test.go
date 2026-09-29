package watch

import (
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
)

func TestTileMarksAndMask(t *testing.T) {
	tile := tile{phase: journal.PhaseDone, number: -20, hasNumber: true, fail: new(-40), joint: []int{-30}, trying: new(-25), anchor: new(-35)}
	for _, tc := range []struct {
		depth int
		want  string
	}{
		{20, "█"}, {25, ">"}, {30, "j"}, {35, "·"}, {40, "x"}, {45, "."},
	} {
		_, got := tile.cellLook(tile.kindAt(tc.depth, true))
		if diff := cmp.Diff(tc.want, got); diff != "" {
			t.Errorf("depth %d glyph (-want +got):\n%s", tc.depth, diff)
		}
	}
	if got := tile.gauge(50); strings.ContainsAny(got, "▒:") {
		t.Errorf("obsolete glyph in gauge %q", got)
	}
}

func TestActivitySummary(t *testing.T) {
	s := Snapshot{huntID: 3, maskID: 4, round: 2, tiles: []tile{{phase: journal.PhaseDone}, {phase: journal.PhaseResident}}}
	want := []string{"hunt 3 mask 4", "refine round 2", "1/2 done", "0 failures", "0 crashes"}
	if diff := cmp.Diff(want, s.summary()); diff != "" {
		t.Errorf("summary (-want +got):\n%s", diff)
	}
}

func TestLoggedKinds(t *testing.T) {
	for _, kind := range []journal.Kind{
		journal.KindHostRanking, journal.KindHuntStart, journal.KindHuntMask, journal.KindHuntEnd,
		journal.KindHuntSkipped, journal.KindMarkJoint, journal.KindRefineRound,
		journal.KindTunerWarning, journal.KindBackendRetry,
	} {
		found := false
		for _, loggedKind := range logged {
			found = found || loggedKind == kind
		}
		if !found {
			t.Errorf("%s absent from recent events", kind)
		}
	}
}
