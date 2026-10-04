package session

import (
	"context"
	"errors"
	"fmt"
	"io"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
)

func TestSameBootInterruptsEverySMURestoreWindow(t *testing.T) {
	for _, pending := range []bool{false, true} {
		t.Run(fmt.Sprintf("dead end %v", pending), func(t *testing.T) {
			reference := sameBootFixture(t, pending)
			var accesses []sim.SMUOperation
			reference.Machine.InterruptSMU(func(access sim.SMUOperation) error { accesses = append(accesses, access); return nil })
			if _, err := resumeStoppedProcess(t, reference, 0); err != nil {
				t.Fatal(err)
			}
			reference.Machine.InterruptSMU(nil)
			want := actualOffsets(t, reference)
			for i, access := range accesses {
				t.Run(fmt.Sprintf("%d %s core %d after %v", i+1, access.Op, access.Core, access.After), func(t *testing.T) {
					testSameBootSMUInterrupt(t, pending, i+1, access, want)
				})
			}
		})
	}
}

func testSameBootSMUInterrupt(t *testing.T, pending bool, at int, access sim.SMUOperation, want []int) {
	t.Helper()
	in := sameBootFixture(t, pending)
	seen, fired := 0, false
	in.Machine.InterruptSMU(func(observed sim.SMUOperation) error {
		seen++
		if !fired && seen == at {
			fired = true
			if diff := cmp.Diff(access, observed); diff != "" {
				t.Fatalf("SMU window (-want +got):\n%s", diff)
			}
			return machine.ErrCrashed
		}
		return nil
	})
	if _, err := resumeStoppedProcess(t, in, 0); !errors.Is(err, machine.ErrCrashed) || !fired {
		t.Fatalf("interruption %v, fired %v", err, fired)
	}
	in.Machine.InterruptSMU(nil)
	if _, err := resumeStoppedProcess(t, in, 0); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(want, actualOffsets(t, in)); diff != "" {
		t.Fatalf("restored target after reopen (-want +got):\n%s", diff)
	}
}

func TestEmergencyZeroingInterruptsEverySMUWindow(t *testing.T) {
	run := func(t *testing.T, at int) ([]sim.SMUOperation, []journal.Payload) {
		t.Helper()
		m := newSim(t, small())
		in := simInput(t.TempDir(), m)
		var faulty *failAppendJournal
		var accesses []sim.SMUOperation
		fired := false
		m.InterruptSMU(func(access sim.SMUOperation) error {
			if faulty == nil || !faulty.failed {
				return nil
			}
			accesses = append(accesses, access)
			if at > 0 && !fired && len(accesses) == at {
				fired = true
				m.Crash()
				return machine.ErrCrashed
			}
			return nil
		})
		_, err := simulateBoot(context.Background(), in, func(j *journal.Journal) Journal {
			faulty = &failAppendJournal{Journal: j, kind: journal.KindTrialStart}
			return faulty
		})
		if !errors.Is(err, io.ErrClosedPipe) || !faulty.failed {
			t.Fatalf("journal failure: %v", err)
		}
		if at > 0 && (!fired || !errors.Is(err, machine.ErrCrashed)) {
			t.Fatalf("emergency interruption: %v, fired %v", err, fired)
		}
		m.InterruptSMU(nil)
		m.NextReset(machine.ResetPowerLoss)
		m.Reboot()
		if stop := simulate(t, in); stop.Reason != StopCycles {
			t.Fatalf("resume after emergency: %+v", stop)
		}
		return accesses, emergencyDecisionFacts(readEvents(t, in.Dir))
	}
	reference, want := run(t, 0)
	for i, access := range reference {
		t.Run(fmt.Sprintf("%d %s core %d after %v", i+1, access.Op, access.Core, access.After), func(t *testing.T) {
			observed, got := run(t, i+1)
			if diff := cmp.Diff(access, observed[i]); diff != "" {
				t.Fatalf("emergency SMU window (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff(want, got); diff != "" {
				t.Fatalf("emergency resumed decisions (-uninterrupted +resumed):\n%s", diff)
			}
		})
	}
}

func emergencyDecisionFacts(events []journal.Event) []journal.Payload {
	var facts []journal.Payload
	for _, event := range events {
		switch event.Data.(type) {
		case *journal.Failure, *journal.TunerDecision, *journal.CorePhase, *journal.Combination, *journal.ProfileChange,
			*journal.HuntStart, *journal.HuntGroup, *journal.HuntEnd, *journal.DeepeningRound, *journal.TrialIntent, *journal.TrialEnd:
			facts = append(facts, event.Data)
		}
	}
	return facts
}
