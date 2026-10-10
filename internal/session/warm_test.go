package session

import (
	"strings"
	"testing"

	"github.com/shgew/togi/internal/journal"
)

func storedWarm(t *testing.T) (*Warm, []journal.Event) {
	t.Helper()
	r, _, closeJournal := checkedRunner(t, []int{0, 0})
	defer closeJournal()
	r.ready = true
	r.in.Warm = new(Warm)
	r.in.Warm.store(r)
	return r.in.Warm, r.in.Journal.Events()
}

func TestWarmVerifyAcceptsTheStateItFolded(t *testing.T) {
	t.Parallel()
	w, events := storedWarm(t)
	if err := w.Verify(events); err != nil {
		t.Fatal(err)
	}
}

func TestWarmVerifyNamesWhatDiffersFromAReplay(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name   string
		tamper func(*Warm)
		want   string
	}{
		{"projection", func(w *Warm) { w.state.Phase = "tampered" }, "projection fields"},
		{"fold", func(w *Warm) { w.fold.baselineSeq++ }, "fold"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			w, events := storedWarm(t)
			tc.tamper(w)
			err := w.Verify(events)
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("Verify = %v, want an error naming %q", err, tc.want)
			}
		})
	}
}

func TestWarmVerifyRejectsAStateThatMissesEvents(t *testing.T) {
	t.Parallel()
	w, events := storedWarm(t)
	if err := w.Verify(append(events, events[len(events)-1])); err == nil {
		t.Fatal("verified a state that folded fewer events than the journal holds")
	}
}

func TestWarmIsAdoptedOnlyWhenItCoversTheJournal(t *testing.T) {
	t.Parallel()
	w, events := storedWarm(t)
	fold := w.fold
	r := &runner{}
	if !w.adopt(r, len(events)) || r.fold != fold || r.folded != len(events) {
		t.Fatal("a state covering every event was not adopted")
	}
	if !w.Empty() {
		t.Fatal("adopted state stayed in the Warm")
	}
	w, events = storedWarm(t)
	if w.adopt(&runner{}, len(events)+1) || !w.Empty() {
		t.Fatal("a state missing events was adopted or kept")
	}
}

func TestWarmStoreDropsStateTheRunnerSetOutsideTheFold(t *testing.T) {
	t.Parallel()
	r, _, closeJournal := checkedRunner(t, []int{0, 0})
	defer closeJournal()
	r.fold.kernelDeadDetail = "kernel log unreadable"
	r.ready = true
	r.in.Warm = new(Warm)
	r.in.Warm.store(r)
	if err := r.in.Warm.Verify(r.in.Journal.Events()); err != nil {
		t.Fatal(err)
	}
}

type countingJournal struct {
	Journal
	writes []journal.State
}

func (j *countingJournal) WriteState(s journal.State) error {
	j.writes = append(j.writes, s)
	return nil
}

func TestDeferredStateIsWrittenBeforeAJournalOnlyAppendAndWhenTheRunEnds(t *testing.T) {
	t.Parallel()
	r, _, closeJournal := checkedRunner(t, []int{0, 0})
	defer closeJournal()
	counting := &countingJournal{Journal: r.in.Journal}
	r.in.Journal, r.in.DeferState = counting, true
	warning := &journal.SessionWarning{Operation: "test"}
	for range 2 {
		if _, err := r.append(warning); err != nil {
			t.Fatal(err)
		}
	}
	if len(counting.writes) != 0 {
		t.Fatalf("%d state writes after two appends, want none", len(counting.writes))
	}
	if _, err := r.appendJournal(warning); err != nil {
		t.Fatal(err)
	}
	if len(counting.writes) != 1 || counting.writes[0].LastSeq != 4 {
		t.Fatalf("state writes %d, want the one pending after event 4", len(counting.writes))
	}
	if err := r.finishState(); err != nil || len(counting.writes) != 1 {
		t.Fatalf("finishState with nothing pending: %v, %d writes", err, len(counting.writes))
	}
	if _, err := r.append(warning); err != nil {
		t.Fatal(err)
	}
	if err := r.finishState(); err != nil {
		t.Fatal(err)
	}
	if len(counting.writes) != 2 || counting.writes[1].LastSeq != 6 {
		t.Fatalf("state writes %d after the run ended, want a second one after event 6", len(counting.writes))
	}
}
