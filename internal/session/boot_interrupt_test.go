package session

import (
	"context"
	"errors"
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/tuner"
)

type effectfulBootloader struct {
	saved             string
	calls             int
	err               error
	effectBeforeError bool
}

func (b *effectfulBootloader) ClearSavedEntry() (string, string, error) {
	b.calls++
	before := b.saved
	if b.err != nil && !b.effectBeforeError {
		return before, before, b.err
	}
	b.saved = ""
	if b.err != nil {
		return before, before, b.err
	}
	return before, "", nil
}

func bootCleanupFixture(t *testing.T) (simRun, int) {
	t.Helper()
	in := sameBootFixture(t, false)
	boot, err := in.Machine.Seams().Host.BootID()
	if err != nil {
		t.Fatal(err)
	}
	j, err := journal.Open(in.Dir, journal.Options{Boot: boot, Now: in.Machine.Now, Build: Build()})
	if err != nil {
		t.Fatal(err)
	}
	event, err := j.Append(&journal.DeadEnd{Condition: journal.DeadEndBootLoop, Action: journal.ActionClearSavedEntryAndReboot, Detail: "interrupted GRUB handoff"}, len(j.Events()))
	if err != nil {
		_ = j.Close()
		t.Fatal(err)
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	return in, event.Seq
}

func TestBootHandoffResumesEveryCleanupBoundary(t *testing.T) {
	cases := []struct {
		name               string
		kind               journal.Kind
		after, beforeUnset bool
		wantBefore         string
		wantCalls          int
		completed          bool
	}{
		{name: "before unset", kind: journal.KindBootSavedEntry, beforeUnset: true, wantBefore: "togi", wantCalls: 2},
		{name: "after unset before entry", kind: journal.KindBootSavedEntry, wantCalls: 2},
		{name: "after entry recorded", kind: journal.KindBootSavedEntry, after: true, wantBefore: "togi", wantCalls: 1},
		{name: "before shutdown", kind: journal.KindShutdown, wantBefore: "togi", wantCalls: 1},
		{name: "after shutdown recorded", kind: journal.KindShutdown, after: true, wantBefore: "togi", wantCalls: 1, completed: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			in, deadSeq := bootCleanupFixture(t)
			want := bootDecisionFacts(t, in)
			bl := &effectfulBootloader{saved: "togi"}
			if tc.beforeUnset {
				bl.err = errKilled
			}
			in.Bootloader = bl
			var gate *appendGate
			ctx, cancel := context.WithCancel(context.Background())
			cancel()
			_, err := simulateBoot(ctx, in, func(j *journal.Journal) Journal {
				gate = &appendGate{match: eventKind(tc.kind), after: tc.after, do: func(journal.Event) error { return errKilled }}
				return &interruptedJournal{Journal: j, gate: gate}
			})
			if !errors.Is(err, errKilled) || gate == nil || !gate.fired {
				t.Fatalf("cleanup boundary not reached: %v", err)
			}
			if (bl.saved == "togi") != tc.beforeUnset {
				t.Fatalf("actual GRUB effect at interruption: saved=%q", bl.saved)
			}
			bl.err = nil
			stop, err := resumeStoppedProcess(t, in, 0)
			if err != nil {
				t.Fatal(err)
			}
			if tc.completed {
				if stop.Reason != StopSignal || stop.Reboot {
					t.Fatalf("completed shutdown authorized another reboot: %+v", stop)
				}
			} else if stop.Reason != StopDeadEnd || !stop.Reboot || stop.DeadEnd.Detail != "interrupted GRUB handoff" {
				t.Fatalf("pending successful handoff not completed: %+v", stop)
			}
			if bl.saved != "" || bl.calls != tc.wantCalls {
				t.Fatalf("idempotent cleanup state=%q calls=%d, want %d", bl.saved, bl.calls, tc.wantCalls)
			}
			entry := assertBootCleanupEvents(t, in, deadSeq)
			if entry.Before != tc.wantBefore || entry.After != "" || entry.Error != "" {
				t.Fatalf("cleanup evidence disagrees with actual resume: %+v", entry)
			}
			if diff := cmp.Diff([]int{-30, -29, -19, -5}, actualOffsets(t, in)); diff != "" {
				t.Fatalf("same-boot restoration bypassed:\n%s", diff)
			}
			if diff := cmp.Diff(want, bootDecisionFacts(t, in)); diff != "" {
				t.Fatalf("GRUB cleanup resume changed decision facts (-before +after):\n%s", diff)
			}
		})
	}
}

func TestFailedBootCleanupNeverAuthorizesRebootOnResume(t *testing.T) {
	for _, effectBeforeError := range []bool{false, true} {
		t.Run(map[bool]string{false: "before unset", true: "after unset"}[effectBeforeError], func(t *testing.T) {
			in, deadSeq := bootCleanupFixture(t)
			want := bootDecisionFacts(t, in)
			cleanupErr := errors.New("grub-editenv unset failed")
			bl := &effectfulBootloader{saved: "togi", err: cleanupErr, effectBeforeError: effectBeforeError}
			in.Bootloader = bl
			var gate *appendGate
			ctx, cancel := context.WithCancel(context.Background())
			cancel()
			_, err := simulateBoot(ctx, in, func(j *journal.Journal) Journal {
				gate = &appendGate{match: eventKind(journal.KindShutdown), do: func(journal.Event) error { return errKilled }}
				return &interruptedJournal{Journal: j, gate: gate}
			})
			if !errors.Is(err, errKilled) || gate == nil || !gate.fired {
				t.Fatalf("shutdown boundary not reached: %v", err)
			}
			stop, err := resumeStoppedProcess(t, in, 0)
			if err != nil || stop.Reason != StopDeadEnd || stop.Reboot || bl.calls != 1 {
				t.Fatalf("failed cleanup reauthorized or repeated: %+v, %v, calls=%d", stop, err, bl.calls)
			}
			entry := assertBootCleanupEvents(t, in, deadSeq)
			if entry.Error != cleanupErr.Error() || entry.Before != "togi" || entry.After != "togi" {
				t.Fatalf("cleanup diagnostic lost: %+v", entry)
			}
			if (bl.saved == "") != effectBeforeError {
				t.Fatalf("failed cleanup actual state changed on resume: %q", bl.saved)
			}
			if diff := cmp.Diff(want, bootDecisionFacts(t, in)); diff != "" {
				t.Fatalf("failed GRUB cleanup resume changed decision facts (-before +after):\n%s", diff)
			}
		})
	}
}

type bootFacts struct {
	Cores      []journal.CoreState
	JointMarks []journal.JointMarkState
	Guard      *journal.GuardState
	Hunt       *journal.HuntState
	Refine     *journal.RefineState
	Evidence   []string
}

func bootDecisionFacts(t *testing.T, in simRun) bootFacts {
	t.Helper()
	events := readEvents(t, in.Dir)
	var state journal.State
	engine := tuner.New()
	journal.Replay(events, &state, engine)
	engine.Project(&state)
	facts := bootFacts{Cores: state.Cores, JointMarks: state.JointMarks, Guard: state.Guard, Hunt: state.Hunt, Refine: state.Refine}
	decisive := []journal.Kind{journal.KindFailure, journal.KindCrashDetected, journal.KindMCE, journal.KindTrialIntent, journal.KindTunerDecision, journal.KindMarkJoint}
	for _, e := range events {
		if slices.Contains(decisive, e.Kind) {
			facts.Evidence = append(facts.Evidence, e.Msg)
		}
	}
	if len(facts.Cores) == 0 || len(facts.JointMarks) == 0 || len(facts.Evidence) == 0 {
		t.Fatalf("decision facts missing from fixture: %+v", facts)
	}
	return facts
}

func assertBootCleanupEvents(t *testing.T, in simRun, deadSeq int) *journal.BootSavedEntry {
	t.Helper()
	var result *journal.BootSavedEntry
	preflight, restored, entry, shutdown, shutdowns := -1, -1, -1, -1, 0
	for _, event := range readEvents(t, in.Dir)[deadSeq:] {
		switch payload := event.Data.(type) {
		case *journal.PreflightCheck:
			if preflight == -1 {
				preflight = event.Seq
			}
		case *journal.ProfileRestored:
			if restored == -1 {
				restored = event.Seq
			}
		case *journal.BootSavedEntry:
			if result != nil || !slices.Equal(event.Cause, []int{deadSeq}) {
				t.Fatalf("duplicate or unrelated GRUB evidence: %+v", event)
			}
			result, entry = payload, event.Seq
		case *journal.Shutdown:
			if payload.Reason == journal.ShutdownDeadEnd {
				shutdown, shutdowns = event.Seq, shutdowns+1
			}
		case *journal.TrialIntent:
			t.Fatal("tuning started before the pending handoff completed")
		}
	}
	if result == nil || shutdowns != 1 || preflight < 0 || restored <= preflight || entry <= restored || shutdown <= entry {
		t.Fatalf("handoff order preflight=%d restore=%d entry=%d shutdown=%d count=%d", preflight, restored, entry, shutdown, shutdowns)
	}
	return result
}
