package watch

import (
	"fmt"
	"strings"
	"testing"

	"github.com/charmbracelet/x/ansi"
	"github.com/google/go-cmp/cmp"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

// A rerun after a backoff names the step checking resumes at: the step of the next cycle trial the journal runs, which
// skips the steps whose chain endings the tuner records without a trial.
func TestRerunNamesTheStepCheckingResumesAt(t *testing.T) {
	t.Parallel()
	events := simulated(t, sessionJournal)
	intents := map[string]*journal.TrialIntent{}
	checked, skipped := 0, 0
	for i, e := range events {
		if p, ok := e.Data.(*journal.TrialIntent); ok {
			intents[p.Trial] = p
		}
		start, ok := e.Data.(*journal.TrialStart)
		if !ok {
			continue
		}
		in := intents[start.Trial]
		if in == nil || !in.Rerun || in.Phase != journal.PhaseChecking || in.Hunt > 0 {
			continue
		}
		next, chains, clean := nextCycleTrial(events[i+1:])
		if next == nil || !clean {
			continue
		}
		s := Project(events[:i+1])
		if s.cycle == nil {
			t.Fatalf("rerun %s has no cycle", in.Trial)
		}
		checked++
		if chains {
			skipped++
		}
		step := stepOfTrial(t, events, next.Trial)
		if diff := cmp.Diff(step, s.cycle.resumeStep()+1); diff != "" {
			t.Errorf("rerun %s at seq %d: step named after the backoff differs from the next cycle trial %s (-next +named):\n%s", in.Trial, e.Seq, next.Trial, diff)
		}
		text := ansi.Strip(Render(s, 240, 67, e.Time))
		if want := fmt.Sprintf("waits at step %d until the rerun passes", step); !strings.Contains(text, want) {
			t.Errorf("rerun %s at seq %d: narration lacks %q", in.Trial, e.Seq, want)
		}
		if want := fmt.Sprintf("paused at step %d of", step); !strings.Contains(text, want) {
			t.Errorf("rerun %s at seq %d: stage line lacks %q", in.Trial, e.Seq, want)
		}
	}
	if checked == 0 || skipped == 0 {
		t.Fatalf("fixture must hold reruns whose next cycle trial skips steps with chain endings: %d reruns, %d with chain endings", checked, skipped)
	}
}

// stepOfTrial is the checking step a trial runs in, as the dashboard shows it while the trial is in flight; the journal
// leaves the step out of the intents of steps the tuner does not number.
func stepOfTrial(t *testing.T, events []journal.Event, trial string) int {
	t.Helper()
	for i, e := range events {
		if p, ok := e.Data.(*journal.TrialStart); ok && p.Trial == trial {
			if s := Project(events[:i+1]); s.trial != nil && s.trial.step > 0 {
				return s.trial.step
			}
		}
	}
	t.Fatalf("trial %s has no step in the journal", trial)
	return 0
}

// nextCycleTrial finds the first non-rerun checking cycle trial in the events, whether chain endings come before it, and
// whether the reruns before it all passed, which is when the journal agrees with the forecast of passing.
func nextCycleTrial(events []journal.Event) (next *journal.TrialIntent, chains, clean bool) {
	clean = true
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.CheckingChain:
			chains = true
		case *journal.Failure, *journal.DeadEnd, *journal.Shutdown:
			clean = false
		case *journal.TrialEnd:
			clean = clean && p.Outcome == journal.OutcomePass
		case *journal.TrialIntent:
			if p.Cycle > 0 && !p.Rerun && p.Hunt == 0 && p.Phase == journal.PhaseChecking {
				return p, chains, clean
			}
			if !p.Rerun {
				return nil, chains, false
			}
		}
	}
	return nil, chains, false
}

// A crash recovery names the step the crashed trial ran in, also after a backoff has moved the cycle on.
func TestRecoveryNamesTheStepTheCrashedTrialRanIn(t *testing.T) {
	t.Parallel()
	events := simulated(t, sessionJournal)
	intents := map[string]*journal.TrialIntent{}
	started := map[string]int{}
	checked, moved := 0, 0
	for i, e := range events {
		switch p := e.Data.(type) {
		case *journal.TrialIntent:
			intents[p.Trial] = p
		case *journal.TrialStart:
			started[p.Trial] = i
		case *journal.TrialEnd:
			in := intents[p.Trial]
			if p.Signal != machine.Crash || in == nil || in.Cycle == 0 || in.Hunt > 0 || in.Round > 0 || in.Rerun {
				continue
			}
			ran := Project(events[:started[p.Trial]+1]).trial.step
			// The recovery shows until the next trial starts, after the backoff the crash caused.
			end := i
			for end+1 < len(events) {
				if _, ok := events[end+1].Data.(*journal.TrialIntent); ok {
					break
				}
				end++
			}
			s := Project(events[:end+1])
			if s.recover == nil || s.recover.trial == nil || s.recover.trial.id != p.Trial {
				continue
			}
			checked++
			if s.cycle != nil && s.cycle.current+1 != ran {
				moved++
			}
			if diff := cmp.Diff(ran, s.recover.trial.step); diff != "" {
				t.Errorf("recovery at seq %d: step of crashed trial %s (-ran +named):\n%s", events[end].Seq, p.Trial, diff)
			}
		}
	}
	if checked == 0 || moved == 0 {
		t.Fatalf("fixture must hold a crashed cycle trial whose cycle moved on: %d recoveries, %d moved", checked, moved)
	}
}
