package session

import (
	"slices"
	"testing"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
)

func TestResumeContinuesBoots(t *testing.T) {
	t.Parallel()
	dir, first := reference(t, small())
	cfg, err := sim.Resume(dir, small())
	if err != nil {
		t.Fatal(err)
	}
	boots := map[string]bool{}
	for _, e := range first {
		boots[e.Boot] = true
	}
	last := first[len(first)-1].Time
	if cfg.Boots != len(boots) || !cfg.Start.After(last) {
		t.Fatalf("Resume: %d boots from %s, want %d after %s", cfg.Boots, cfg.Start, len(boots), last)
	}
	in := simInput(dir, newSim(t, cfg))
	in.Rotations = 2
	if stop := simulate(t, in); stop.Reason != StopRotations {
		t.Fatalf("second run stopped with %+v", stop)
	}
	events := readEvents(t, dir)
	if len(events) == len(first) {
		t.Fatal("second run appended nothing")
	}
	for _, e := range events[len(first):] {
		if boots[e.Boot] {
			t.Fatalf("seq %d reuses boot %s", e.Seq, e.Boot)
		}
		if e.Time.Before(last) {
			t.Fatalf("seq %d at %s, before %s", e.Seq, e.Time, last)
		}
		last = e.Time
	}
}

// A boot's kernel log starts with the MCEs its predecessor's crash left in the banks. Once the predecessor's
// crash.detected cites them, they are no evidence of the later boot's own crash.
func TestCrashEvidenceIsClaimedOnce(t *testing.T) {
	t.Parallel()
	f := newFold()
	for _, e := range []journal.Event{
		{Seq: 1, Boot: "a", Kind: journal.KindSessionStart, Data: &journal.SessionStart{}},
		{Seq: 2, Boot: "b", Kind: journal.KindMCE, Data: &journal.MCE{Core: 3, BankType: machine.LoadStore, FromBoot: "b", Lines: []string{"left by a"}}},
		{Seq: 3, Boot: "b", Kind: journal.KindCrashDetected, Data: &journal.CrashDetected{PreviousBoot: "a"}, Cause: []int{2}},
	} {
		f.Fold(e)
	}
	if got := f.recordedFor("a", "b"); !slices.Equal(got, []int{2}) {
		t.Fatalf("evidence of boot a: %v, want [2]", got)
	}
	if got := f.recordedFor("b", "c"); len(got) != 0 {
		t.Fatalf("evidence of boot b: %v, want none", got)
	}
}
