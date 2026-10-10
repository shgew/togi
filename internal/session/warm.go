package session

import (
	"encoding/json"
	"errors"
	"fmt"
	"reflect"
	"strings"

	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/tuner"
)

// Warm is what a run folded from its journal, kept for the next run on the same journal so that it resumes without
// replaying the events it already folded. The simulator uses it across simulated crashes; `togi run` replays at every
// boot.
//
// The zero value holds nothing. A run adopts the state only when it covers exactly the events the journal holds, and
// leaves the Warm empty either way; every run that folded events stores its state back when it returns.
type Warm struct {
	fold    *fold
	state   journal.State
	tuner   *tuner.State
	defects defect.Index
	events  int
	dirty   bool
	// pending marks the projection as not yet written to the journal's state file.
	pending bool
}

// Empty reports whether w holds no state to adopt.
func (w *Warm) Empty() bool { return w == nil || w.fold == nil }

func (w *Warm) project() {
	if w.dirty {
		w.tuner.Project(&w.state)
		w.dirty = false
	}
}

// Verify replays events into fresh state and compares it with the state w holds: the fold the run loop reads, the state
// projection, the next tuner action and the defect findings. Any difference is an error naming what differs.
func (w *Warm) Verify(events []journal.Event) error {
	if w.Empty() {
		return errors.New("no warm state to verify")
	}
	if w.events != len(events) {
		return fmt.Errorf("warm state folded %d events, the journal holds %d", w.events, len(events))
	}
	w.project()
	fresh := replay(events)
	fresh.project()
	var problems []string
	if fields := journal.DiffFields(w.state, fresh.state); len(fields) > 0 {
		problems = append(problems, fmt.Sprintf("projection fields %v", fields))
	} else if !reflect.DeepEqual(w.state, fresh.state) {
		problems = append(problems, "projection")
	}
	if !reflect.DeepEqual(w.fold, fresh.fold) {
		problems = append(problems, "fold")
	}
	if got, want := nextAction(w.tuner), nextAction(fresh.tuner); !reflect.DeepEqual(got, want) {
		problems = append(problems, fmt.Sprintf("next tuner action: warm %+v, replayed %+v", got, want))
	}
	entries := defect.Entries()
	if got, want := findings(w.defects.Find(events, entries)), findings(defect.FindWith(events, entries)); got != want {
		problems = append(problems, fmt.Sprintf("defect findings: warm %s, replayed %s", got, want))
	}
	if rt, err := stateRoundTrip(w.state); err != nil {
		problems = append(problems, err.Error())
	} else if fields := journal.DiffFields(rt, w.state); len(fields) > 0 {
		problems = append(problems, fmt.Sprintf("projection fields %v change in a JSON round trip", fields))
	}
	if len(problems) > 0 {
		return fmt.Errorf("warm state differs from a replay of %d events: %s", len(events), strings.Join(problems, "; "))
	}
	return nil
}

// nextAction is what the tuner would do next, or its panic: a journal cut before the session is set up has no next action,
// and the warm state must agree on that too.
func nextAction(t *tuner.State) (outcome any) {
	defer func() {
		if p := recover(); p != nil {
			outcome = fmt.Sprint("panic: ", p)
		}
	}()
	return t.Next()
}

// findings renders what each finding says about the journal; an entry's predicates are functions and do not compare.
func findings(list []defect.Finding) string {
	var b strings.Builder
	for _, f := range list {
		fmt.Fprintf(&b, "[defect %d cores %v decisions %v]", f.Entry.ID, f.Cores, f.Decisions)
	}
	return b.String()
}

func replay(events []journal.Event) *Warm {
	w := &Warm{fold: newFold(), tuner: tuner.New(), events: len(events), dirty: true}
	journal.Replay(events, w.fold, &w.state, w.tuner)
	return w
}

// stateRoundTrip is the state as a state file written from s reads back. A boot that adopts a Warm whose state is still
// unwritten skips the comparison of that file with the state, which holds only while the two agree in every field.
func stateRoundTrip(s journal.State) (journal.State, error) {
	data, err := json.Marshal(s)
	if err != nil {
		return journal.State{}, fmt.Errorf("marshal state: %w", err)
	}
	var out journal.State
	if err := json.Unmarshal(data, &out); err != nil {
		return journal.State{}, fmt.Errorf("unmarshal state: %w", err)
	}
	return out, nil
}

// adopt takes w's state into r when it covers events, and reports whether it did. Whatever w held is gone either way.
// State a crashed run left unwritten that r does not adopt is written to the journal first.
func (w *Warm) adopt(r *runner, events int) (bool, error) {
	if w == nil || w.Empty() {
		return false, nil
	}
	defer func() { *w = Warm{} }()
	if w.events != events {
		if !w.pending {
			return false, nil
		}
		w.project()
		return false, r.in.Journal.WriteState(w.state)
	}
	r.fold, r.state, r.tuner, r.defects, r.folded = w.fold, w.state, w.tuner, w.defects, w.events
	r.dirty, r.statePending = w.dirty, w.pending
	return true, nil
}

// store keeps r's state in w for the next run.
func (w *Warm) store(r *runner) {
	// The runner sets kernelDeadDetail outside the fold; no event records it, so a replay never has it.
	r.fold.kernelDeadDetail = ""
	*w = Warm{fold: r.fold, state: r.state, tuner: r.tuner, defects: r.defects, events: r.folded, dirty: r.dirty, pending: r.statePending}
}

// TakePending returns the state projection a crashed run left unwritten, and that it did leave one. The caller writes
// it to the journal, as the run would have.
func (w *Warm) TakePending() (journal.State, bool) {
	if w.Empty() || !w.pending {
		return journal.State{}, false
	}
	w.project()
	w.pending = false
	return w.state, true
}

// recordedBuild is journal.BuildOf the journal's events, read from the folded state when w covers all of them.
func (w *Warm) recordedBuild(events int) (journal.Build, bool) {
	if w == nil || w.Empty() || w.events != events || w.fold.build == nil {
		return journal.Build{}, false
	}
	return *w.fold.build, true
}
