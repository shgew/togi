package session

import (
	"fmt"
	"slices"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

func crashTrialEvents(events []journal.Event) []int {
	var positions []int
	for i, e := range events {
		intent, ok := e.Data.(*journal.TrialIntent)
		if !ok {
			continue
		}
		for _, next := range events[i:] {
			if next.Boot != e.Boot {
				break
			}
			positions = append(positions, next.Seq)
			if end, ok := next.Data.(*journal.TrialEnd); ok && end.Trial == intent.Trial {
				if end.Outcome == journal.OutcomeFailure && next.Seq < len(events) && events[next.Seq].Kind == journal.KindFailure {
					positions = append(positions, next.Seq+1)
				}
				break
			}
		}
	}
	slices.Sort(positions)
	return slices.Compact(positions)
}

func TestCrashAtEveryTrialEvent(t *testing.T) {
	t.Parallel()
	_, ref := reference(t, small())
	for _, k := range crashTrialEvents(ref) {
		t.Run(fmt.Sprint(k), func(t *testing.T) {
			t.Parallel()
			in := simInput(t.TempDir(), newSim(t, small()))
			if stop := drive(t, in, crashAt(k, in.Machine)); stop.Reason != StopRotations {
				t.Fatalf("stopped with %+v", stop)
			}
			events := readEvents(t, in.Dir)
			hit := events[k-1]
			detected, ok := crashDetectedFor(events, hit.Boot)
			if !ok {
				t.Fatalf("no crash.detected for boot of seq %d", k)
			}
			var intent *journal.TrialIntent
			var ended bool
			var started, last time.Duration
			for _, e := range events[:k] {
				switch p := e.Data.(type) {
				case *journal.TrialIntent:
					intent, ended, started, last = p, false, 0, 0
				case *journal.TrialStart:
					started, last = time.Duration(e.Mono)*time.Millisecond, time.Duration(e.Mono)*time.Millisecond
				case *journal.TrialProgress, *journal.TrialSignal, *journal.TrialSample:
					if started != 0 {
						last = time.Duration(e.Mono) * time.Millisecond
					}
				case *journal.TrialEnd:
					if intent != nil && p.Trial == intent.Trial {
						ended = true
					}
				}
			}
			if intent == nil {
				t.Fatalf("crash at seq %d without trial.intent", k)
			}
			if !ended {
				var closed *journal.TrialEnd
				for _, e := range events[k:] {
					if p, ok := e.Data.(*journal.TrialEnd); ok && p.Trial == intent.Trial {
						closed = p
						break
					}
				}
				if closed == nil {
					t.Fatalf("trial %s left open after crash at %s", intent.Trial, hit.Kind)
				}
				if got, want := closed.DurationS, int((last - started).Seconds()); got != want {
					t.Fatalf("trial %s duration %d, want %d from boot-local time", intent.Trial, got, want)
				}
				if closed.Outcome != journal.OutcomeFailure {
					t.Fatalf("trial %s ended %+v, want failure", intent.Trial, closed)
				}
			}
			failure := failureCiting(events, detected.Seq)
			if failure == nil {
				var same []journal.Event
				for _, e := range events {
					if f, ok := e.Data.(*journal.Failure); ok && f.Trial == intent.Trial {
						same = append(same, e)
					}
				}
				if len(same) != 1 {
					t.Fatalf("crash at %s seq %d: no failure cites crash #%d, trial failures %+v", hit.Kind, k, detected.Seq, same)
				}
				failure = same[0].Data.(*journal.Failure)
			}
			if !ended && failure.Trial != intent.Trial {
				t.Fatalf("crash at %s: failure %+v does not cite trial %s", hit.Kind, failure, intent.Trial)
			}
			if !ended && intent.Condition == machine.Isolated {
				if failure.Core == nil || *failure.Core != *intent.Core || failure.Offset == nil || *failure.Offset != *intent.Offset {
					t.Fatalf("isolated crash attribution %+v, intent %+v", failure, intent)
				}
			}
			if !ended && intent.Condition == machine.Resident && failure.Signal == machine.Crash && failure.Attribution != journal.Unattributed {
				t.Fatalf("resident crash attribution %+v", failure)
			}
			if diff := cmp.Diff([]string(nil), in.Machine.Violations()); diff != "" {
				t.Fatalf("simulator isolation violations (-want +got):\n%s", diff)
			}
			marks := tuner.New()
			for _, e := range events {
				if p, ok := e.Data.(*journal.TrialIntent); ok && p.Condition != machine.Isolated {
					if mark, reaches := marks.Reaches(p.Profile); reaches {
						t.Fatalf("trial %s reaches %s", p.Trial, mark)
					}
				}
				marks.Fold(e)
			}
		})
	}
}
