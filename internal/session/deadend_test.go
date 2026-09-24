package session

import (
	"context"
	"errors"
	"slices"
	"testing"

	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
	"code.marleb.org/shgew/shycler/internal/sim"
)

type fakeBootloader struct {
	calls int
	err   error
}

func (b *fakeBootloader) ClearSavedEntry() (string, string, error) {
	b.calls++
	if b.err != nil {
		return "shycler", "shycler", b.err
	}
	return "shycler", "", nil
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
