package session

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
)

type killedProcessJournal struct {
	Journal
	at    int
	fired bool
	reads int
}

func (j *killedProcessJournal) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := j.Journal.Append(p, cause...)
	if p, ok := p.(*journal.SMUReadback); ok && p.Expected == nil {
		j.reads++
	}
	if err == nil && !j.fired && (e.Seq == j.at || (j.at == -1 && j.reads == 4)) {
		j.fired = true
		return e, machine.ErrCrashed
	}
	return e, err
}

func sameBootFixture(t *testing.T, pending bool) simRun {
	t.Helper()
	cfg := sim.Config{Seed: 84, Cores: 4, BIOS: []int{-40, -40, -20, -5}, Model: &sim.Model{}}
	in := simInput(t.TempDir(), newSim(t, cfg))
	seams := in.Machine.Seams()
	boot, err := seams.Host.BootID()
	if err != nil {
		t.Fatal(err)
	}
	cores, err := seams.Host.Topology()
	if err != nil {
		t.Fatal(err)
	}
	bios, err := seams.Host.BIOSContext()
	if err != nil {
		t.Fatal(err)
	}
	j, err := journal.Open(in.Dir, journal.Options{Boot: boot, Now: in.Machine.Now, Build: Build()})
	if err != nil {
		t.Fatal(err)
	}
	payloads := []journal.Payload{
		&journal.SessionStart{Build: Build(), Session: "same-boot", Cores: cores},
		&journal.SessionContext{BIOSContext: bios},
		&journal.SessionBaseline{Offsets: cfg.BIOS},
		&journal.CorePhase{Core: 0, To: journal.PhaseResident, Offset: -30},
		&journal.CorePhase{Core: 1, To: journal.PhaseResident, Offset: -29},
		&journal.CorePhase{Core: 2, To: journal.PhaseResident, Offset: -19, FailedMark: new(-20)},
		&journal.CorePhase{Core: 3, To: journal.PhaseResident, Offset: -5},
		&journal.MarkJoint{Mark: 1, Members: []journal.JointMember{{Core: 0, Offset: -30}, {Core: 1, Offset: -30}}},
		&journal.ProfileApplied{Offsets: []int{-45, -45, -25, -12}, Condition: machine.Resident},
	}
	if pending {
		payloads = append(payloads, &journal.DeadEnd{Condition: journal.DeadEndNoEvidence, Action: journal.ActionClearSavedEntry, Detail: "recorded backend failure"})
		in.Bootloader = &fakeBootloader{}
	}
	for _, p := range payloads {
		if _, err := j.Append(p); err != nil {
			t.Fatal(err)
		}
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	for core, offset := range []int{-45, -45, -25, -12} {
		if err := seams.SMU.SetOffset(core, offset); err != nil {
			t.Fatal(err)
		}
	}
	return in
}

func actualOffsets(t *testing.T, in simRun) []int {
	t.Helper()
	cores, err := in.Machine.Seams().Host.Topology()
	if err != nil {
		t.Fatal(err)
	}
	offsets := make([]int, len(cores))
	for i, core := range cores {
		offsets[i], err = in.Machine.Seams().SMU.Offset(core.Core)
		if err != nil {
			t.Fatal(err)
		}
	}
	return offsets
}

func resumeStoppedProcess(t *testing.T, in simRun, at int) (Stop, error) {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	return simulateBoot(ctx, in, func(j *journal.Journal) Journal {
		return &killedProcessJournal{Journal: wrapFor(in, nil)(j), at: at}
	})
}

func TestSameBootRestoresBeforeDeadEndShutdown(t *testing.T) {
	t.Parallel()
	in := sameBootFixture(t, true)
	before := len(readEvents(t, in.Dir))
	stop, err := resumeStoppedProcess(t, in, 0)
	if err != nil || stop.Reason != StopDeadEnd || stop.DeadEnd.Detail != "recorded backend failure" {
		t.Fatalf("resume: %+v, %v", stop, err)
	}
	if diff := cmp.Diff([]int{-30, -29, -19, -5}, actualOffsets(t, in)); diff != "" {
		t.Fatalf("actual offsets before dead-end shutdown (-want +got):\n%s", diff)
	}
	events := readEvents(t, in.Dir)[before:]
	preflight, restored, entry, shutdown := -1, -1, -1, -1
	for i, e := range events {
		switch e.Data.(type) {
		case *journal.PreflightCheck:
			preflight = i
		case *journal.ProfileRestored:
			restored = i
		case *journal.BootSavedEntry:
			entry = i
		case *journal.Shutdown:
			shutdown = i
		case *journal.SMUIntent:
			if preflight < 0 {
				t.Fatal("restoration before preflight")
			}
		case *journal.TrialIntent:
			t.Fatal("started a trial before finishing the pending dead end")
		}
	}
	if preflight < 0 || restored <= preflight || entry <= restored || shutdown <= entry {
		t.Fatalf("startup order: preflight %d, restored %d, saved entry %d, shutdown %d", preflight, restored, entry, shutdown)
	}
}

func TestSameBootKillAtEveryRestoreEvent(t *testing.T) {
	t.Parallel()
	for _, pending := range []bool{false, true} {
		t.Run(fmt.Sprintf("pending_dead_end_%v", pending), func(t *testing.T) {
			t.Parallel()
			ref := sameBootFixture(t, pending)
			before := len(readEvents(t, ref.Dir))
			if _, err := resumeStoppedProcess(t, ref, 0); err != nil {
				t.Fatal(err)
			}
			events := readEvents(t, ref.Dir)[before:]
			restored := slices.IndexFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindProfileRestored })
			if restored < 0 {
				t.Fatal("resume did not restore the journal safe target")
			}
			for _, e := range events[:restored+1] {
				if e.Kind != journal.KindSMUReadback && e.Kind != journal.KindSMUIntent && e.Kind != journal.KindSMUWrite && e.Kind != journal.KindProfileRestored {
					continue
				}
				t.Run(fmt.Sprintf("%d_%s", e.Seq, e.Kind), func(t *testing.T) {
					t.Parallel()
					in := sameBootFixture(t, pending)
					if _, err := resumeStoppedProcess(t, in, e.Seq); !errors.Is(err, machine.ErrCrashed) {
						t.Fatalf("SIGKILL-equivalent at %d: %v", e.Seq, err)
					}
					for range 2 {
						start := len(readEvents(t, in.Dir))
						if _, err := resumeStoppedProcess(t, in, -1); !errors.Is(err, machine.ErrCrashed) {
							t.Fatalf("repeated SIGKILL-equivalent during readback: %v", err)
						}
						if readEvents(t, in.Dir)[start].Boot != e.Boot {
							t.Fatal("process interruption changed the boot")
						}
					}
					if _, err := resumeStoppedProcess(t, in, 0); err != nil {
						t.Fatal(err)
					}
					if diff := cmp.Diff([]int{-30, -29, -19, -5}, actualOffsets(t, in)); diff != "" {
						t.Fatalf("same-boot restoration (-want +got):\n%s", diff)
					}
				})
			}
		})
	}
}

func TestRebootSkipsSameBootReconciliation(t *testing.T) {
	t.Parallel()
	for _, pending := range []bool{false, true} {
		t.Run(fmt.Sprintf("pending_dead_end_%v", pending), func(t *testing.T) {
			in := sameBootFixture(t, pending)
			before := len(readEvents(t, in.Dir))
			in.Machine.Reboot()
			if _, err := resumeStoppedProcess(t, in, 0); err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff([]int{-40, -40, -20, -5}, actualOffsets(t, in)); diff != "" {
				t.Fatalf("firmware-reset offsets (-want +got):\n%s", diff)
			}
			for _, e := range readEvents(t, in.Dir)[before:] {
				if e.Kind == journal.KindSMUReadback || e.Kind == journal.KindSMUIntent || e.Kind == journal.KindProfileRestored {
					t.Fatalf("reboot performed same-boot reconciliation: %s", e.Kind)
				}
			}
		})
	}
}

func TestSameBootResumeDrainsPendingFailureBeforeRestore(t *testing.T) {
	t.Parallel()
	in := sameBootFixture(t, true)
	boot, err := in.Machine.Seams().Host.BootID()
	if err != nil {
		t.Fatal(err)
	}
	j, err := journal.Open(in.Dir, journal.Options{Boot: boot, Now: in.Machine.Now, Build: Build()})
	if err != nil {
		t.Fatal(err)
	}
	phase := &journal.CorePhase{Core: 2, From: journal.PhaseResident, To: journal.PhaseSearch, Offset: -15}
	if _, err := j.Append(phase); err != nil {
		t.Fatal(err)
	}
	failure, err := j.Append(&journal.Failure{Signal: machine.ComputationError, Attribution: journal.Attributed, Core: new(2), Offset: new(-15), Condition: machine.Isolated})
	if err != nil {
		t.Fatal(err)
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := resumeStoppedProcess(t, in, 0); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]int{-30, -29, -10, -5}, actualOffsets(t, in)); diff != "" {
		t.Fatalf("pending failure restore (-want +got):\n%s", diff)
	}
	events := readEvents(t, in.Dir)[failure.Seq:]
	backoff := slices.IndexFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindTunerDecision })
	intent := slices.IndexFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindSMUIntent })
	if backoff < 0 || intent <= backoff {
		t.Fatalf("pending backoff %d must precede restoration intent %d", backoff, intent)
	}
}

func TestSameBootResumeRejectsUnvalidatedSMU(t *testing.T) {
	t.Parallel()
	for _, check := range []string{"cpu", "ryzen_smu"} {
		t.Run(check, func(t *testing.T) {
			in := sameBootFixture(t, true)
			in.Machine.FailCheck(check, "unsupported")
			seams := in.Machine.Seams()
			seams.SMU = forbiddenSMU{t}
			boot, err := seams.Host.BootID()
			if err != nil {
				t.Fatal(err)
			}
			j, err := journal.Open(in.Dir, journal.Options{Boot: boot, Now: in.Machine.Now, Build: Build()})
			if err != nil {
				t.Fatal(err)
			}
			stop, err := Run(context.Background(), Input{Config: in.Config, Boot: boot, Journal: wrapFor(in, nil)(j), Machine: seams})
			if closeErr := j.Close(); closeErr != nil {
				t.Fatal(closeErr)
			}
			if err != nil || stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != journal.DeadEndPreflight {
				t.Fatalf("unsupported %s resume: %+v, %v", check, stop, err)
			}
			if diff := cmp.Diff([]int{-45, -45, -25, -12}, actualOffsets(t, in)); diff != "" {
				t.Fatalf("unsupported hardware changed offsets (-want +got):\n%s", diff)
			}
			for _, e := range readEvents(t, in.Dir) {
				if e.Kind == journal.KindShutdown {
					t.Fatal("recorded a clean shutdown without reconciling the same-boot offsets")
				}
			}
		})
	}
}
