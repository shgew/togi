package defect

import (
	"slices"
	"testing"
	"time"

	"code.marleb.org/shgew/shycler/internal/journal"
)

func fixture(t *testing.T) []journal.Event {
	t.Helper()
	events, torn, err := journal.ReadFile("testdata/power-off.jsonl")
	if err != nil || len(torn) != 0 {
		t.Fatalf("read pre-fix journal: %v, %d torn bytes", err, len(torn))
	}
	return events
}

func TestPowerOffDefect(t *testing.T) {
	for _, tc := range []struct {
		name string
		edit func([]journal.Event)
		want bool
	}{
		{"pre-fix power-off and unrelated unexpected exit", nil, true},
		{"fixed before decision", func(e []journal.Event) { e[1].Data.(*journal.ConfigLoaded).Fixes = 1 }, false},
		{"shutdown outside five seconds", func(e []journal.Event) { e[5].Time = e[2].Time.Add(6 * time.Second) }, false},
		{"shutdown from another boot", func(e []journal.Event) { e[5].Boot = "boot-b" }, false},
		{"unrelated shutdown reason", func(e []journal.Event) { e[5].Data.(*journal.Shutdown).Reason = journal.ShutdownRotations }, false},
		{"decision does not cite the failure", func(e []journal.Event) { e[4].Cause = []int{3} }, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			events := fixture(t)
			if tc.edit != nil {
				tc.edit(events)
			}
			got := Find(events)
			if !tc.want {
				if len(got) != 0 {
					t.Fatalf("unexpected findings: %+v", got)
				}
				return
			}
			if len(got) != 1 || got[0].Entry.ID != 1 || !slices.Equal(got[0].Cores, []int{10}) || !slices.Equal(got[0].Decisions, []int{5}) {
				t.Fatalf("findings %+v; want defect 1, only core 10 and decision #5", got)
			}
			events = append(events, journal.Event{Seq: 11, Kind: journal.KindDefectFound, Data: &journal.DefectFound{ID: 1}})
			if got := Find(events); len(got) != 0 {
				t.Fatalf("second resume repeated finding: %+v", got)
			}
		})
	}
}
