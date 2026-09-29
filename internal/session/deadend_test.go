package session

import (
	"context"
	"errors"
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
)

type fakeBootloader struct {
	calls int
	err   error
}

func (b *fakeBootloader) ClearSavedEntry() (string, string, error) {
	b.calls++
	if b.err != nil {
		return "togi", "togi", b.err
	}
	return "togi", "", nil
}

func TestDeadEndClearsSavedEntry(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name   string
		fault  func(*sim.Machine)
		err    error
		action journal.DeadEndAction
		reboot bool
	}{
		{name: "smu", fault: func(m *sim.Machine) { m.CorruptReadback(0) }, action: journal.ActionClearSavedEntry},
		{name: "boot loop", fault: func(m *sim.Machine) { m.CrashBeforeApply(3) }, action: journal.ActionClearSavedEntryAndReboot, reboot: true},
		{name: "failed clear", fault: func(m *sim.Machine) { m.CrashBeforeApply(3) }, err: errors.New("grub-editenv: exit status 1"), action: journal.ActionClearSavedEntryAndReboot},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			in := simInput(t.TempDir(), newSim(t, small()))
			tt.fault(in.Machine)
			bl := &fakeBootloader{err: tt.err}
			in.Bootloader = bl
			stop := simulate(t, in)
			if stop.Reason != StopDeadEnd || stop.DeadEnd.Action != tt.action || stop.Reboot != tt.reboot {
				t.Fatalf("stopped with %+v, want action %s, reboot %v", stop, tt.action, tt.reboot)
			}
			if bl.calls != 1 {
				t.Fatalf("ClearSavedEntry called %d times", bl.calls)
			}
			events := readEvents(t, in.Dir)
			tail := events[len(events)-3:]
			kinds := []journal.Kind{tail[0].Kind, tail[1].Kind, tail[2].Kind}
			if !slices.Equal(kinds, []journal.Kind{journal.KindDeadEnd, journal.KindBootSavedEntry, journal.KindShutdown}) {
				t.Fatalf("journal ends with %v", kinds)
			}
			entry := tail[1].Data.(*journal.BootSavedEntry)
			if !slices.Equal(tail[1].Cause, []int{tail[0].Seq}) {
				t.Fatalf("boot.saved_entry cause %v, want the dead end %d", tail[1].Cause, tail[0].Seq)
			}
			if (entry.Error != "") != (tt.err != nil) {
				t.Fatalf("boot.saved_entry %+v with clear error %v", entry, tt.err)
			}
		})
	}
}

func TestResumeInterruptedDeadEnd(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name   string
		fault  func(*sim.Machine)
		action journal.DeadEndAction
		err    error
		reboot bool
	}{
		{"clear", func(m *sim.Machine) { m.CorruptReadback(0) }, journal.ActionClearSavedEntry, nil, false},
		{"clear and reboot", func(m *sim.Machine) { m.CrashBeforeApply(3) }, journal.ActionClearSavedEntryAndReboot, nil, true},
		{"failed clear", func(m *sim.Machine) { m.CrashBeforeApply(3) }, journal.ActionClearSavedEntryAndReboot, errors.New("grub-editenv: exit status 1"), false},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			reference := simInput(t.TempDir(), newSim(t, small()))
			tt.fault(reference.Machine)
			reference.Bootloader = &fakeBootloader{}
			simulate(t, reference)
			refEvents := readEvents(t, reference.Dir)
			deadSeq := refEvents[slices.IndexFunc(refEvents, func(e journal.Event) bool { return e.Kind == journal.KindDeadEnd })].Seq

			in := simInput(t.TempDir(), newSim(t, small()))
			tt.fault(in.Machine)
			bl := &fakeBootloader{err: tt.err}
			in.Bootloader = bl
			stop := drive(t, in, killAt(deadSeq))
			if stop.Reason != StopDeadEnd || stop.DeadEnd.Action != tt.action || stop.Reboot != tt.reboot {
				t.Fatalf("resumed stop %+v, want dead end action %s, reboot %v", stop, tt.action, tt.reboot)
			}
			if bl.calls != 1 {
				t.Fatalf("clear called %d times, want once on resume", bl.calls)
			}
			events := readEvents(t, in.Dir)
			dead := slices.IndexFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindDeadEnd })
			if dead < 0 {
				t.Fatal("no deadend")
			}
			resumed := deadEndResumeKinds(t, events, events[dead].Seq)
			if !slices.Equal(resumed, []journal.Kind{journal.KindBootSavedEntry, journal.KindShutdown}) {
				t.Fatalf("resume events after dead end: %v", resumed)
			}
			entry := events[slices.IndexFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindBootSavedEntry })].Data.(*journal.BootSavedEntry)
			if (entry.Error != "") != (tt.err != nil) {
				t.Fatalf("recorded clear error %q, expected error %v", entry.Error, tt.err)
			}
		})
	}
}

func TestResumeAfterSavedEntryRecorded(t *testing.T) {
	t.Parallel()
	reference := simInput(t.TempDir(), newSim(t, small()))
	reference.Machine.CorruptReadback(0)
	reference.Bootloader = &fakeBootloader{}
	simulate(t, reference)
	events := readEvents(t, reference.Dir)
	entry := events[slices.IndexFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindBootSavedEntry })]

	in := simInput(t.TempDir(), newSim(t, small()))
	in.Machine.CorruptReadback(0)
	bl := &fakeBootloader{}
	in.Bootloader = bl
	stop := drive(t, in, killAt(entry.Seq))
	if stop.Reason != StopDeadEnd || bl.calls != 1 {
		t.Fatalf("resumed stop %+v after %d clears", stop, bl.calls)
	}
	events = readEvents(t, in.Dir)
	if events[len(events)-1].Kind != journal.KindShutdown {
		t.Fatalf("last event %s, want shutdown", events[len(events)-1].Kind)
	}
	for _, kind := range deadEndResumeKinds(t, events, entry.Seq) {
		if kind != journal.KindShutdown {
			t.Fatalf("repeated tuning or GRUB action after saved entry: %s", kind)
		}
	}
}

func TestResumeInterruptedDeadEndOutsideTuningBoot(t *testing.T) {
	t.Parallel()
	reference := simInput(t.TempDir(), newSim(t, small()))
	reference.Machine.CorruptReadback(0)
	reference.Bootloader = &fakeBootloader{}
	simulate(t, reference)
	refEvents := readEvents(t, reference.Dir)
	deadSeq := refEvents[slices.IndexFunc(refEvents, func(e journal.Event) bool { return e.Kind == journal.KindDeadEnd })].Seq

	in := simInput(t.TempDir(), newSim(t, small()))
	in.Machine.CorruptReadback(0)
	bl := &fakeBootloader{}
	in.Bootloader = bl
	if _, err := simulateBoot(context.Background(), in, wrapFor(in, killAt(deadSeq))); !errors.Is(err, errKilled) {
		t.Fatalf("first boot: %v, want killed at deadend", err)
	}
	in.Bootloader = nil
	stop := simulate(t, in)
	if stop.Reason != StopDeadEnd || stop.DeadEnd.Action != journal.ActionClearSavedEntry || stop.Reboot || bl.calls != 0 {
		t.Fatalf("resumed stop %+v after %d clears", stop, bl.calls)
	}
	events := readEvents(t, in.Dir)
	resumed := deadEndResumeKinds(t, events, deadSeq)
	if !slices.Equal(resumed, []journal.Kind{journal.KindShutdown}) {
		t.Fatalf("resume events after dead end: %v", resumed)
	}
	if stop := simulate(t, in); stop.Reason != StopRotations {
		t.Fatalf("next run did not resume tuning: %+v", stop)
	}
}

func TestFullyRecordedDeadEndReevaluatesPreflight(t *testing.T) {
	t.Parallel()
	in := simInput(t.TempDir(), newSim(t, small()))
	in.Machine.FailCheck("root", "uid 1000")
	bl := &fakeBootloader{}
	in.Bootloader = bl
	if stop := simulate(t, in); stop.Reason != StopDeadEnd {
		t.Fatalf("first stop %+v", stop)
	}
	before := len(readEvents(t, in.Dir))
	if stop := simulate(t, in); stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != journal.DeadEndPreflight {
		t.Fatalf("second stop %+v", stop)
	}
	if bl.calls != 2 {
		t.Fatalf("clear called %d times after two dead ends", bl.calls)
	}
	for _, e := range readEvents(t, in.Dir)[before:] {
		if e.Kind == journal.KindSMUIntent || e.Kind == journal.KindTrialIntent {
			t.Fatalf("tuning with failing preflight after complete dead end: %s", e.Kind)
		}
	}
}

func TestDeadEndWithoutBootloaderExits(t *testing.T) {
	t.Parallel()
	in := simInput(t.TempDir(), newSim(t, small()))
	in.Machine.CrashBeforeApply(3)
	stop := simulate(t, in)
	if stop.Reason != StopDeadEnd || stop.DeadEnd.Action != journal.ActionExit || stop.Reboot {
		t.Fatalf("stopped with %+v", stop)
	}
}

type reportingTrials struct{ machine.Trials }

func (t reportingTrials) Start(ctx context.Context, spec machine.TrialSpec) (machine.Running, error) {
	r, err := t.Trials.Start(ctx, spec)
	if err != nil {
		return nil, err
	}
	return reportingRunning{r}, nil
}

type reportingRunning struct{ machine.Running }

func (r reportingRunning) Wait(ctx context.Context, report machine.Reporter) (machine.Result, error) {
	report.Progress("x")
	report.Sample(machine.Sample{Warning: "w", PID: 1, TID: 2, CPU: 3})
	return r.Running.Wait(ctx, report)
}

func TestReporterRecordsProgress(t *testing.T) {
	t.Parallel()
	in := simInput(t.TempDir(), newSim(t, small()))
	seams := in.Machine.Seams()
	seams.Trials = reportingTrials{seams.Trials}
	boot, _ := seams.Host.BootID()
	j, err := journal.Open(in.Dir, journal.Options{Boot: boot, Now: in.Machine.Now})
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	jr := &progressCancel{Journal: wrapFor(in, nil)(j), cancel: cancel}
	if _, err := Run(ctx, Input{Config: in.Config, ConfigPath: in.ConfigPath, Boot: boot, Journal: jr, Machine: seams}); err != nil && !errors.Is(err, machine.ErrCrashed) {
		t.Fatal(err)
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	events := readEvents(t, in.Dir)
	start := slices.IndexFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindTrialStart })
	if start < 0 || start+2 >= len(events) {
		t.Fatal("no trial.start followed by the reports")
	}
	progress, sample := events[start+1], events[start+2]
	if p, ok := progress.Data.(*journal.TrialProgress); !ok || p.Detail != "x" || !slices.Equal(progress.Cause, []int{events[start].Seq}) {
		t.Fatalf("after trial.start: %s %s cause %v", progress.Kind, progress.Msg, progress.Cause)
	}
	if s, ok := sample.Data.(*journal.TrialSample); !ok || s.CPU != 3 || !slices.Equal(sample.Cause, []int{events[start].Seq}) {
		t.Fatalf("then: %s %s cause %v", sample.Kind, sample.Msg, sample.Cause)
	}
}

// progressCancel stops the run after the first trial.end, so the test needs one trial only.
type progressCancel struct {
	Journal
	cancel context.CancelFunc
}

func (j *progressCancel) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := j.Journal.Append(p, cause...)
	if e.Kind == journal.KindTrialEnd {
		j.cancel()
	}
	return e, err
}

func deadEndResumeKinds(t *testing.T, events []journal.Event, after int) []journal.Kind {
	t.Helper()
	baseline := 0
	for _, e := range events {
		if e.Kind == journal.KindSessionBaseline {
			baseline = e.Seq
		}
	}
	var kinds []journal.Kind
	preflight := false
	finished := false
	for _, e := range events[after:] {
		switch e.Data.(type) {
		case *journal.StateRebuilt:
		case *journal.PreflightCheck:
			preflight = true
		case *journal.SMUReadback, *journal.SMUWrite, *journal.ProfileRestored:
			if !preflight || finished {
				t.Fatalf("reconciliation outside the preflight/completion boundary: %s", e.Kind)
			}
		case *journal.SMUIntent:
			if !preflight || finished || baseline == 0 || !slices.Contains(e.Cause, baseline) {
				t.Fatalf("non-restoration write after dead end: %+v", e)
			}
		default:
			finished = true
			kinds = append(kinds, e.Kind)
		}
	}
	return kinds
}

func assertOnlyRestorationWrites(t *testing.T, events []journal.Event, after int) {
	t.Helper()
	baseline := 0
	for _, e := range events {
		if e.Kind == journal.KindSessionBaseline {
			baseline = e.Seq
		}
	}
	for _, e := range events[after:] {
		if e.Kind == journal.KindSMUIntent && (baseline == 0 || !slices.Contains(e.Cause, baseline)) {
			t.Fatalf("non-restoration write after dead-end evidence: %+v", e)
		}
		if e.Kind == journal.KindTrialIntent {
			t.Fatalf("trial after dead-end evidence: %+v", e)
		}
	}
}

type containmentTrials struct {
	machine.Trials
	atStart bool
}

func (t containmentTrials) Start(ctx context.Context, spec machine.TrialSpec) (machine.Running, error) {
	if t.atStart {
		return nil, errors.Join(errors.New("second instance failed; output still open"), machine.ErrContainment)
	}
	r, err := t.Trials.Start(ctx, spec)
	if err != nil {
		return nil, err
	}
	return containmentRunning{r}, nil
}

type containmentRunning struct{ machine.Running }

func (r containmentRunning) Wait(ctx context.Context, report machine.Reporter) (machine.Result, error) {
	result, err := r.Running.Wait(ctx, report)
	result.Signal = machine.ComputationError
	return result, errors.Join(err, context.Canceled, machine.ErrContainment)
}

func TestUnconfirmedCleanupDeadEnd(t *testing.T) {
	for _, atStart := range []bool{false, true} {
		name := "normal end"
		if atStart {
			name = "partial start"
		}
		t.Run(name, func(t *testing.T) {
			run := func(in simRun, killed *trigger) (Stop, error) {
				seams := in.Machine.Seams()
				seams.Trials = containmentTrials{Trials: seams.Trials, atStart: atStart}
				boot, _ := seams.Host.BootID()
				j, err := journal.Open(in.Dir, journal.Options{Boot: boot, Now: in.Machine.Now, Monotonic: seams.Clock.Monotonic})
				if err != nil {
					t.Fatal(err)
				}
				defer j.Close()
				return Run(context.Background(), Input{Config: in.Config, ConfigPath: in.ConfigPath, Boot: boot, Journal: wrapFor(in, killed)(j), Machine: seams})
			}
			in := simInput(t.TempDir(), newSim(t, small()))
			stop, err := run(in, nil)
			if err != nil || stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != journal.DeadEndContainment {
				t.Fatalf("cleanup session stop %+v, error %v", stop, err)
			}
			events := readEvents(t, in.Dir)
			endIndex := slices.IndexFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindTrialEnd })
			if endIndex < 0 {
				t.Fatal("cleanup has no trial end evidence")
			}
			end := events[endIndex].Data.(*journal.TrialEnd)
			if end.Outcome != journal.OutcomeInconclusive || end.ContainmentError == "" || end.Signal != "" {
				t.Fatalf("cleanup was adjudicated as stability evidence: %+v", end)
			}
			if events[endIndex+1].Kind != journal.KindDeadEnd {
				t.Fatalf("offset write or decision before containment dead end: %s", events[endIndex+1].Kind)
			}
			for _, e := range events[endIndex+1:] {
				if slices.Contains([]journal.Kind{journal.KindTrialIntent, journal.KindProfileApplied, journal.KindProfileChange, journal.KindFailure}, e.Kind) {
					t.Fatalf("tuning continued after failed cleanup: %s", e.Kind)
				}
			}
			if diff := cmp.Diff([]int{events[endIndex].Seq}, events[endIndex+1].Cause); diff != "" {
				t.Fatalf("containment dead end cause (-want +got):\n%s", diff)
			}
			resumed := simInput(t.TempDir(), newSim(t, small()))
			if _, err := run(resumed, killAt(events[endIndex].Seq)); !errors.Is(err, errKilled) {
				t.Fatalf("interrupt after cleanup evidence = %v", err)
			}
			if stop := simulate(t, resumed); stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != journal.DeadEndContainment {
				t.Fatalf("resume lost cleanup containment evidence: %+v", stop)
			}
			t.Logf("%s: containment dead end, no next trial/profile change; interrupted evidence resumes to the same dead end", name)
		})
	}
}
