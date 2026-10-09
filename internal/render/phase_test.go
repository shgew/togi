package render

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
)

func TestPhaseWord(t *testing.T) {
	t.Parallel()
	got := map[journal.Phase]string{}
	for _, p := range []journal.Phase{journal.PhaseSearch, journal.PhaseHasRoom, journal.PhaseAtLimit, "unknown"} {
		got[p] = PhaseWord(p)
	}
	want := map[journal.Phase]string{journal.PhaseSearch: "SEARCH", journal.PhaseHasRoom: "HAS ROOM", journal.PhaseAtLimit: "AT LIMIT", "unknown": "unknown"}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("words (-want +got):\n%s", diff)
	}
}
