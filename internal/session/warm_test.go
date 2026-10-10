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
	if adopted, err := w.adopt(r, len(events)); err != nil || !adopted || r.fold != fold || r.folded != len(events) {
		t.Fatalf("a state covering every event: adopted %t, %v", adopted, err)
	}
	if !w.Empty() {
		t.Fatal("adopted state stayed in the Warm")
	}
	w, events = storedWarm(t)
	if adopted, err := w.adopt(&runner{}, len(events)+1); err != nil || adopted || !w.Empty() {
		t.Fatalf("a state missing events: adopted %t, %v, kept %t", adopted, err, !w.Empty())
	}
}

func TestCrashLeavesDeferredStateUnwrittenInWarm(t *testing.T) {
	t.Parallel()
	r, _, closeJournal := checkedRunner(t, []int{0, 0})
	defer closeJournal()
	counting := &countingJournal{Journal: r.in.Journal}
	r.in.Journal, r.in.DeferState, r.in.Warm = counting, true, new(Warm)
	r.ready = true
	if _, err := r.append(&journal.SessionWarning{Operation: "test"}); err != nil {
		t.Fatal(err)
	}
	r.in.Warm.store(r)
	if len(counting.writes) != 0 {
		t.Fatalf("%d state writes by a run that crashed", len(counting.writes))
	}
	next := &runner{in: Input{Journal: counting}}
	if adopted, err := r.in.Warm.adopt(next, len(counting.Events())); err != nil || !adopted {
		t.Fatalf("adopted %t, %v", adopted, err)
	}
	if !next.statePending {
		t.Fatal("the adopted state forgot that it is unwritten")
	}
	if err := next.checkState(); err != nil || len(counting.writes) != 0 {
		t.Fatalf("checkState of an unwritten adopted state: %v, %d writes", err, len(counting.writes))
	}
	if err := next.finishState(); err != nil || len(counting.writes) != 1 || counting.writes[0].LastSeq != 3 {
		t.Fatalf("finishing the run: %v, writes %+v", err, counting.writes)
	}
}

func TestPendingStateIsTakenOnceAndWrittenWhenNotAdopted(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name string
		take func(*Warm, *countingJournal, int) journal.State
	}{
		{"taken", func(w *Warm, _ *countingJournal, _ int) journal.State {
			state, ok := w.TakePending()
			if !ok {
				t.Error("no pending state")
			}
			if _, again := w.TakePending(); again {
				t.Error("pending state taken twice")
			}
			return state
		}},
		{"journal grew", func(w *Warm, j *countingJournal, events int) journal.State {
			if adopted, err := w.adopt(&runner{in: Input{Journal: j}}, events+1); err != nil || adopted {
				t.Errorf("adopted %t, %v", adopted, err)
			}
			if len(j.writes) != 1 {
				t.Fatalf("%d state writes, want the pending one", len(j.writes))
			}
			return j.writes[0]
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			r, _, closeJournal := checkedRunner(t, []int{0, 0})
			defer closeJournal()
			counting := &countingJournal{Journal: r.in.Journal}
			r.in.Journal, r.in.DeferState, r.in.Warm = counting, true, new(Warm)
			r.ready = true
			if _, err := r.append(&journal.SessionWarning{Operation: "test"}); err != nil {
				t.Fatal(err)
			}
			r.in.Warm.store(r)
			events := len(counting.Events())
			if state := tc.take(r.in.Warm, counting, events); state.LastSeq != events {
				t.Fatalf("state as of event %d, want %d", state.LastSeq, events)
			}
		})
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
