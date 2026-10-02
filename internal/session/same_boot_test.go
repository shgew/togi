package session

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"testing"
	"time"

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
	cfg := sim.Config{Seed: 84, Cores: 4, BIOS: []int{-40, -40, -20, -5}, Model: quietModel()}
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

func TestCanceledSameBootResumeStillRequiresArmedWatchdog(t *testing.T) {
	t.Parallel()
	in := sameBootFixture(t, true)
	in.Machine.FailCheck("watchdog", "unarmed")
	before := len(readEvents(t, in.Dir))
	start := in.Machine.Monotonic()
	stop, err := resumeStoppedProcess(t, in, 0)
	if err != nil || stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != journal.DeadEndPreflight {
		t.Fatalf("canceled resume with unarmed watchdog: %+v, %v", stop, err)
	}
	if diff := cmp.Diff(30*time.Second, in.Machine.Monotonic()-start); diff != "" {
		t.Fatalf("bounded watchdog wait (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff([]int{-45, -45, -25, -12}, actualOffsets(t, in)); diff != "" {
		t.Fatalf("unarmed watchdog changed offsets (-want +got):\n%s", diff)
	}
	for _, e := range readEvents(t, in.Dir)[before:] {
		if e.Kind == journal.KindSMUIntent || e.Kind == journal.KindSMUReadback || e.Kind == journal.KindProfileRestored || e.Kind == journal.KindTrialIntent || e.Kind == journal.KindShutdown {
			t.Fatalf("unsafe action before watchdog readiness: %+v", e)
		}
	}
}

func TestCanceledRecoveryReconcilesInheritedOffsetsBeforeSignalShutdown(t *testing.T) {
	t.Parallel()
	in, _ := firstCrash(t, machine.ResetWatchdog, machine.Crash, false)
	seams := in.Machine.Seams()
	clock := &retryClock{Clock: seams.Clock}
	seams.Clock = clock
	seams.Kernel = &unreadableKernel{Kernel: seams.Kernel, failUntil: 1}
	boot, err := seams.Host.BootID()
	if err != nil {
		t.Fatal(err)
	}
	j, err := journal.Open(in.Dir, journal.Options{Boot: boot, Now: in.Machine.Now, Monotonic: seams.Clock.Monotonic, Build: Build()})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := j.Append(&journal.ConfigLoaded{Build: Build(), Config: configSnapshot(in.Config)}); err != nil {
		t.Fatal(err)
	}
	for core, offset := range []int{-30, -20} {
		if err := seams.SMU.SetOffset(core, offset); err != nil {
			t.Fatal(err)
		}
	}
	before := len(j.Events())
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	stop, runErr := Run(ctx, Input{Config: in.Config, Boot: boot, Journal: wrapFor(in, nil)(j), Machine: seams, Bootloader: &fakeBootloader{}})
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	if runErr != nil || stop.Reason != StopSignal {
		t.Fatalf("canceled recovery: %+v, %v", stop, runErr)
	}
	if diff := cmp.Diff([]int{-10, -10}, actualOffsets(t, in)); diff != "" {
		t.Fatalf("inherited offsets after canceled recovery (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff([]time.Duration{time.Minute}, clock.waits); diff != "" {
		t.Fatalf("cancelable kernel recovery waits (-want +got):\n%s", diff)
	}
	events := readEvents(t, in.Dir)[before:]
	restored := slices.IndexFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindProfileRestored })
	shutdown := slices.IndexFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindShutdown })
	if restored < 0 || shutdown <= restored {
		t.Fatalf("signal shutdown %d before inherited-offset restoration %d", shutdown, restored)
	}
	for _, e := range events {
		if e.Kind == journal.KindTrialIntent {
			t.Fatal("started a trial after canceled recovery")
		}
	}
}

func TestPendingBootLoopSurvivesFailedSameBootPreflight(t *testing.T) {
	t.Parallel()
	for _, check := range []string{"watchdog", "root"} {
		t.Run(check, func(t *testing.T) {
			t.Parallel()
			in := sameBootFixture(t, false)
			bl := &fakeBootloader{}
			in.Bootloader = bl
			boot, err := in.Machine.Seams().Host.BootID()
			if err != nil {
				t.Fatal(err)
			}
			j, err := journal.Open(in.Dir, journal.Options{Boot: boot, Now: in.Machine.Now, Build: Build()})
			if err != nil {
				t.Fatal(err)
			}
			var evidence []int
			for i := range 3 {
				e, err := j.Append(&journal.CrashDetected{PreviousBoot: fmt.Sprintf("prior-%d", i), Stray: true})
				if err != nil {
					t.Fatal(err)
				}
				evidence = append(evidence, e.Seq)
			}
			original, err := j.Append(&journal.DeadEnd{Condition: journal.DeadEndBootLoop, Action: journal.ActionClearSavedEntryAndReboot, Detail: "three crashes before application"}, evidence...)
			if err != nil {
				t.Fatal(err)
			}
			if err := j.Close(); err != nil {
				t.Fatal(err)
			}
			in.Machine.FailCheck(check, "temporarily unavailable")
			blocked, err := resumeStoppedProcess(t, in, 0)
			if err != nil || blocked.Reason != StopDeadEnd || blocked.DeadEnd.Condition != journal.DeadEndPreflight {
				t.Fatalf("temporarily blocked preflight: %+v, %v", blocked, err)
			}
			for _, e := range readEvents(t, in.Dir)[original.Seq:] {
				if e.Kind == journal.KindDeadEnd || e.Kind == journal.KindBootSavedEntry || e.Kind == journal.KindShutdown || e.Kind == journal.KindSMUIntent {
					t.Fatalf("failed preflight replaced or completed the original boot-loop action: %s", e.Msg)
				}
			}
			if bl.calls != 0 {
				t.Fatalf("cleared the saved entry %d times before reconciliation", bl.calls)
			}
			if diff := cmp.Diff([]int{-45, -45, -25, -12}, actualOffsets(t, in)); diff != "" {
				t.Fatalf("blocked preflight changed inherited offsets (-want +got):\n%s", diff)
			}
			in.Machine.FailCheck(check, "")
			stop, err := resumeStoppedProcess(t, in, 0)
			if err != nil || stop.Reason != StopDeadEnd || !stop.Reboot {
				t.Fatalf("ready restart lost the original reboot: %+v, %v", stop, err)
			}
			if diff := cmp.Diff(original.Data.(*journal.DeadEnd), stop.DeadEnd); diff != "" {
				t.Fatalf("original pending dead end (-want +got):\n%s", diff)
			}
			var gotEvidence []int
			for _, e := range stop.Evidence {
				gotEvidence = append(gotEvidence, e.Seq)
			}
			if diff := cmp.Diff(evidence, gotEvidence); diff != "" {
				t.Fatalf("original pending evidence (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff([]int{-30, -29, -19, -5}, actualOffsets(t, in)); diff != "" {
				t.Fatalf("actual offsets before original reboot action (-want +got):\n%s", diff)
			}
			if bl.calls != 1 {
				t.Fatalf("original action cleared the saved entry %d times, want once", bl.calls)
			}
		})
	}
}

func TestSameBootPreflightDefersNewDeadEndAction(t *testing.T) {
	t.Parallel()
	in := sameBootFixture(t, false)
	bl := &fakeBootloader{}
	in.Bootloader = bl
	in.Machine.FailCheck("root", "temporarily unavailable")
	before := len(readEvents(t, in.Dir))
	stop, err := resumeStoppedProcess(t, in, 0)
	if err != nil || stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != journal.DeadEndPreflight {
		t.Fatalf("new preflight failure: %+v, %v", stop, err)
	}
	events := readEvents(t, in.Dir)[before:]
	var pending *journal.Event
	for i, e := range events {
		if e.Kind == journal.KindDeadEnd {
			if pending != nil {
				t.Fatal("recorded more than one pending preflight dead end")
			}
			pending = &events[i]
		}
		if e.Kind == journal.KindBootSavedEntry || e.Kind == journal.KindShutdown || e.Kind == journal.KindSMUIntent {
			t.Fatalf("preflight failure completed an action before reconciliation: %s", e.Msg)
		}
	}
	if pending == nil || bl.calls != 0 {
		t.Fatalf("pending preflight dead end %+v, saved-entry clears %d", pending, bl.calls)
	}
	if diff := cmp.Diff([]int{-45, -45, -25, -12}, actualOffsets(t, in)); diff != "" {
		t.Fatalf("failed preflight changed offsets (-want +got):\n%s", diff)
	}
	in.Machine.FailCheck("root", "")
	stop, err = resumeStoppedProcess(t, in, 0)
	if err != nil || stop.Reason != StopDeadEnd {
		t.Fatalf("ready resume: %+v, %v", stop, err)
	}
	if diff := cmp.Diff(pending.Data.(*journal.DeadEnd), stop.DeadEnd); diff != "" {
		t.Fatalf("new pending preflight outcome (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff([]int{-30, -29, -19, -5}, actualOffsets(t, in)); diff != "" {
		t.Fatalf("actual offsets before saved-entry clear (-want +got):\n%s", diff)
	}
	if bl.calls != 1 {
		t.Fatalf("saved-entry clears %d, want one after reconciliation", bl.calls)
	}
}
