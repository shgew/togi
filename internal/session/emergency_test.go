package session

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
)

type failAppendJournal struct {
	Journal
	kind   journal.Kind
	at     int
	calls  int
	failed bool
}

func (j *failAppendJournal) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	j.calls++
	if !j.failed && ((j.at != 0 && j.calls == j.at) || (j.at == 0 && p.Kind() == j.kind)) {
		j.failed = true
		return journal.Event{}, io.ErrClosedPipe
	}
	return j.Journal.Append(p, cause...)
}

func TestJournalFailureZerosMachineAndStopsAppending(t *testing.T) {
	for _, kind := range []journal.Kind{journal.KindTrialStart, journal.KindTrialProgress} {
		t.Run(string(kind), func(t *testing.T) {
			cfg := small()
			cfg.Script = map[string]sim.Outcome{"0001": {Signal: machine.ComputationError, AtS: 1, Core: 0}}
			m := newSim(t, cfg)
			in := simInput(t.TempDir(), m)
			var stderr bytes.Buffer
			in.Stderr = &stderr
			var faulty *failAppendJournal
			_, err := simulateBoot(context.Background(), in, func(j *journal.Journal) Journal {
				faulty = &failAppendJournal{Journal: j, kind: kind}
				return faulty
			})
			if !faulty.failed || !errors.Is(err, io.ErrClosedPipe) || !strings.HasPrefix(err.Error(), "journal write: ") {
				t.Fatalf("journal failure %+v, err %v", faulty, err)
			}
			want := "togi: journal write failed: io: read/write on closed pipe; every core set to CO 0 without an intent (readback all 0)\n"
			if got := stderr.String(); got != want {
				t.Fatalf("stderr mismatch (-want +got):\n%s", fmt.Sprintf("%q != %q", got, want))
			}
			for core := range cfg.BIOS {
				got, err := m.Seams().SMU.Offset(core)
				if err != nil || got != 0 {
					t.Errorf("core %d reads %d (%v), want 0", core, got, err)
				}
			}
			events := readEvents(t, in.Dir)
			for _, e := range events {
				if e.Kind == kind || e.Kind == journal.KindTrialEnd {
					t.Errorf("event after failed append: %s", e.Kind)
				}
			}
		})
	}
}

type failEmergencyZero struct {
	machine.SMU
	journal *failAppendJournal
	failed  bool
}

func (s *failEmergencyZero) SetAllOffsets(offset int) error {
	if s.journal.failed && !s.failed {
		s.failed = true
		return errors.New("simulated emergency zeroing failure")
	}
	return s.SMU.SetAllOffsets(offset)
}

func TestJournalFailureReportsFailedEmergencyZeroAndReadbacks(t *testing.T) {
	t.Parallel()
	cfg := small()
	cfg.BIOS = []int{-10, -20}
	m := newSim(t, cfg)
	in := simInput(t.TempDir(), m)
	var stderr bytes.Buffer
	seams := m.Seams()
	boot, err := seams.Host.BootID()
	if err != nil {
		t.Fatal(err)
	}
	j, err := journal.Open(in.Dir, journal.Options{Boot: boot, Now: m.Now, Monotonic: m.Monotonic, Build: Build()})
	if err != nil {
		t.Fatal(err)
	}
	faulty := &failAppendJournal{Journal: wrapFor(in, nil)(j), kind: journal.KindSessionStart}
	smu := &failEmergencyZero{SMU: seams.SMU, journal: faulty}
	seams.SMU = smu
	_, err = Run(context.Background(), Input{Config: in.Config, ConfigPath: in.ConfigPath, Boot: boot, Journal: faulty, Machine: seams, Rotations: 1, Stderr: &stderr})
	if closeErr := j.Close(); closeErr != nil {
		t.Fatal(closeErr)
	}
	if !faulty.failed || !smu.failed || !errors.Is(err, io.ErrClosedPipe) {
		t.Fatalf("journal failure: journal failed %v, emergency zero failed %v, err %v", faulty.failed, smu.failed, err)
	}
	var readbacks []string
	for core := range cfg.BIOS {
		offset, readErr := m.Seams().SMU.Offset(core)
		if readErr != nil {
			t.Fatal(readErr)
		}
		readbacks = append(readbacks, fmt.Sprintf("core %02d reads %d", core, offset))
	}
	want := "togi: journal write failed: io: read/write on closed pipe; setting every core to CO 0 without an intent failed: simulated emergency zeroing failure (readback: " + strings.Join(readbacks, ", ") + ")\n"
	if diff := cmp.Diff(want, stderr.String()); diff != "" {
		t.Fatalf("stderr (-want +got):\n%s", diff)
	}
}

type unwritableState struct{ Journal }

func (unwritableState) ReadState() (journal.State, error) {
	return journal.State{}, errors.New("no state file")
}
func (unwritableState) WriteState(journal.State) error { return errors.New("simulated full disk") }

func TestStateRewriteFailureOnResumeRestoresSafeOffsets(t *testing.T) {
	t.Parallel()
	cfg := small()
	cfg.BIOS = []int{-10, -20}
	cfg.Model = quietModel()
	m := newSim(t, cfg)
	in := simInput(t.TempDir(), m)
	in.Config.CandidateEdges = map[int]int{0: -50, 1: -50}
	simulate(t, in)
	before := len(readEvents(t, in.Dir))
	m.Reboot()
	var stderr bytes.Buffer
	in.Stderr = &stderr
	stop, err := simulateBoot(context.Background(), in, func(j *journal.Journal) Journal {
		return unwritableState{wrapFor(in, nil)(j)}
	})
	if err != nil || stop.Reason != StopRotations {
		t.Fatalf("projection failure stopped resumed tuning: stop %+v, error %v", stop, err)
	}
	if diff := cmp.Diff("", stderr.String()); diff != "" {
		t.Fatalf("projection failure invoked emergency zeroing (-want +got):\n%s", diff)
	}
	warning := readEvents(t, in.Dir)[before]
	if diff := cmp.Diff(&journal.SessionWarning{Operation: "write state projection", Error: "simulated full disk"}, warning.Data); diff != "" {
		t.Fatalf("resume did not warn about the failed projection: %s", diff)
	}
	for core, want := range cfg.BIOS {
		got, err := m.Seams().SMU.Offset(core)
		if err != nil || got != want {
			t.Fatalf("core %d reads %d (%v), want ordinary restore %d", core, got, err, want)
		}
	}
}

type trackedTrials struct {
	machine.Trials
	active *int
}

func (t trackedTrials) Start(ctx context.Context, spec machine.TrialSpec) (machine.Running, error) {
	r, err := t.Trials.Start(ctx, spec)
	if err != nil {
		return nil, err
	}
	*t.active++
	return trackedRunning{Running: r, active: t.active}, nil
}

type trackedRunning struct {
	machine.Running
	active *int
}

func (r trackedRunning) Wait(ctx context.Context, report machine.Reporter) (machine.Result, error) {
	defer func() { *r.active-- }()
	return r.Running.Wait(ctx, report)
}

func TestEveryEarlyJournalAppendFailureZerosMachine(t *testing.T) {
	t.Parallel()
	cfg := small()
	cfg.BIOS = []int{-10, -10}
	cfg.Script = map[string]sim.Outcome{"0001": {Signal: machine.ComputationError, AtS: 1, Core: 0}}
	ref := simInput(t.TempDir(), newSim(t, cfg))
	if stop := simulate(t, ref); stop.Reason != StopRotations {
		t.Fatalf("reference stopped with %+v", stop)
	}
	events := readEvents(t, ref.Dir)
	progress := -1
	for i, e := range events {
		if p, ok := e.Data.(*journal.TrialProgress); ok && p.Signal == machine.ComputationError {
			progress = i
			break
		}
	}
	if progress < 0 {
		t.Fatal("reference run lacks in-Wait progress")
	}
	for k := 1; k <= progress+1; k++ {
		t.Run(fmt.Sprint(k), func(t *testing.T) {
			t.Parallel()
			m := newSim(t, cfg)
			in := simInput(t.TempDir(), m)
			var stderr bytes.Buffer
			seams := m.Seams()
			active := 0
			seams.Trials = trackedTrials{Trials: seams.Trials, active: &active}
			boot, err := seams.Host.BootID()
			if err != nil {
				t.Fatal(err)
			}
			j, err := journal.Open(in.Dir, journal.Options{Boot: boot, Now: m.Now, Monotonic: m.Monotonic, Build: Build()})
			if err != nil {
				t.Fatal(err)
			}
			faulty := &failAppendJournal{Journal: wrapFor(in, nil)(j), at: k}
			_, err = Run(context.Background(), Input{Config: in.Config, ConfigPath: in.ConfigPath, Boot: boot, Journal: faulty, Machine: seams, Rotations: 1, Stderr: &stderr})
			if closeErr := j.Close(); closeErr != nil {
				t.Fatal(closeErr)
			}
			if !faulty.failed || !errors.Is(err, io.ErrClosedPipe) {
				t.Fatalf("k-th append failure: fired %v, err %v", faulty.failed, err)
			}
			wantStderr := "togi: journal write failed: io: read/write on closed pipe; every core set to CO 0 without an intent (readback all 0)\n"
			if diff := cmp.Diff(wantStderr, stderr.String()); diff != "" {
				t.Fatalf("stderr (-want +got):\n%s", diff)
			}
			if active != 0 {
				t.Fatalf("%d trial backends still running", active)
			}
			for core := range cfg.BIOS {
				got, err := m.Seams().SMU.Offset(core)
				if err != nil || got != 0 {
					t.Fatalf("core %d after append failure: %d, %v", core, got, err)
				}
			}
			written := readEvents(t, in.Dir)
			if got := len(written); got != k-1 {
				t.Fatalf("%d events written after failed append #%d; want %d", got, k, k-1)
			}
			for i, e := range written {
				if e.Seq != i+1 || e.Kind != events[i].Kind {
					t.Fatalf("event #%d: seq %d kind %s, want seq %d kind %s", i+1, e.Seq, e.Kind, i+1, events[i].Kind)
				}
			}
		})
	}
}
