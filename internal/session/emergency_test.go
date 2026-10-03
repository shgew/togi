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
	"github.com/shgew/togi/internal/tuner"
)

type failAppendJournal struct {
	Journal
	kind   journal.Kind
	at     int
	check  string
	calls  int
	failed bool
}

func (j *failAppendJournal) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	j.calls++
	matches := p.Kind() == j.kind
	if j.check != "" {
		check, ok := p.(*journal.PreflightCheck)
		matches = ok && check.Check == j.check
	}
	if !j.failed && ((j.at != 0 && j.calls == j.at) || (j.at == 0 && matches)) {
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

type forbiddenSMU struct{ t *testing.T }

func (s forbiddenSMU) Offset(int) (int, error) {
	s.t.Fatal("SMU read before validation")
	return 0, nil
}

func (s forbiddenSMU) SetOffset(int, int) error {
	s.t.Fatal("SMU write before validation")
	return nil
}

func (s forbiddenSMU) SetAllOffsets(int) error {
	s.t.Fatal("SMU all-core write before validation")
	return nil
}

func TestEarlyJournalFailureDoesNotAccessUnvalidatedSMU(t *testing.T) {
	t.Parallel()
	m := newSim(t, small())
	in := simInput(t.TempDir(), m)
	seams := m.Seams()
	seams.SMU = forbiddenSMU{t}
	j, err := journal.Open(in.Dir, journal.Options{Boot: "test", Build: Build()})
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	faulty := &failAppendJournal{Journal: j, kind: journal.KindSessionStart}
	_, err = Run(context.Background(), Input{Config: in.Config, Boot: "test", Journal: faulty, Machine: seams})
	if !errors.Is(err, io.ErrClosedPipe) {
		t.Fatalf("journal failure lost: %v", err)
	}
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
	faulty := &failAppendJournal{Journal: wrapFor(in, nil)(j), kind: journal.KindTrialStart}
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
		if offset != 0 {
			readbacks = append(readbacks, fmt.Sprintf("core %02d reads %d", core, offset))
		}
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
	swept  *bool
}

func (t trackedTrials) Sweep(ctx context.Context) (string, error) {
	detail, err := t.Trials.Sweep(ctx)
	if t.swept != nil {
		*t.swept = err == nil
	}
	return detail, err
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

func (r trackedRunning) Stop() error {
	defer func() { *r.active-- }()
	return r.Running.Stop()
}

func TestEveryEarlyJournalAppendFailureRespectsSweepGate(t *testing.T) {
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
	sweepSeq := 0
	for i, e := range events {
		if p, ok := e.Data.(*journal.PreflightCheck); ok && p.Check == "trial_scopes" {
			sweepSeq = e.Seq
		}
		if p, ok := e.Data.(*journal.TrialProgress); ok && p.Signal == machine.ComputationError {
			progress = i
			break
		}
	}
	if progress < 0 || sweepSeq == 0 {
		t.Fatal("reference run lacks sweep or in-Wait progress")
	}
	for k := 1; k <= progress+1; k++ {
		t.Run(fmt.Sprint(k), func(t *testing.T) {
			t.Parallel()
			m := newSim(t, cfg)
			in := simInput(t.TempDir(), m)
			var stderr bytes.Buffer
			seams := m.Seams()
			active := 0
			o := &sweepObservation{t: t}
			seams.SMU = sweepSMU{SMU: seams.SMU, o: o}
			seams.Trials = trackedTrials{Trials: seams.Trials, active: &active, swept: &o.swept}
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
			if k < sweepSeq {
				wantStderr = "togi: journal write failed: io: read/write on closed pipe; offsets unchanged before successful trial-scope sweep\n"
			}
			if diff := cmp.Diff(wantStderr, stderr.String()); diff != "" {
				t.Fatalf("stderr (-want +got):\n%s", diff)
			}
			if active != 0 {
				t.Fatalf("%d trial backends still running", active)
			}
			for core, baseline := range cfg.BIOS {
				want := 0
				if k < sweepSeq {
					want = baseline
				}
				got, err := m.Seams().SMU.Offset(core)

				if err != nil || got != want {
					t.Fatalf("core %d after append failure: %d, %v; want %d", core, got, err, want)
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

type liveContainmentTrials struct {
	machine.Trials
	live *bool
}

func (t liveContainmentTrials) Start(ctx context.Context, spec machine.TrialSpec) (machine.Running, error) {
	running, err := t.Trials.Start(ctx, spec)
	if err != nil {
		return nil, err
	}
	*t.live = true
	return liveContainmentRunning{Running: running}, nil
}

type liveContainmentRunning struct{ machine.Running }

func (r liveContainmentRunning) Wait(_ context.Context, report machine.Reporter) (machine.Result, error) {
	report.Progress("backend still running")
	return machine.Result{}, fmt.Errorf("backend still running after teardown: %w", machine.ErrContainment)
}

func (r liveContainmentRunning) Stop() error {
	return fmt.Errorf("backend still running after teardown: %w", machine.ErrContainment)
}

type liveWriteSMU struct {
	machine.SMU
	live   *bool
	writes int
}

func (s *liveWriteSMU) SetOffset(core, offset int) error {
	if *s.live {
		s.writes++
	}
	return s.SMU.SetOffset(core, offset)
}

func (s *liveWriteSMU) SetAllOffsets(offset int) error {
	if *s.live {
		s.writes++
	}
	return s.SMU.SetAllOffsets(offset)
}

func TestJournalFailureWithUnconfirmedTeardownWithholdsAllWrites(t *testing.T) {
	for _, kind := range []journal.Kind{journal.KindTrialProgress, journal.KindTrialEnd} {
		t.Run(string(kind), func(t *testing.T) {
			t.Parallel()
			m := newSim(t, small())
			in := simInput(t.TempDir(), m)
			seams := m.Seams()
			live := false
			smu := &liveWriteSMU{SMU: seams.SMU, live: &live}
			seams.SMU = smu
			seams.Trials = liveContainmentTrials{Trials: seams.Trials, live: &live}
			boot, err := seams.Host.BootID()
			if err != nil {
				t.Fatal(err)
			}
			j, err := journal.Open(in.Dir, journal.Options{Boot: boot, Now: m.Now, Monotonic: m.Monotonic, Build: Build()})
			if err != nil {
				t.Fatal(err)
			}
			defer j.Close()
			faulty := &failAppendJournal{Journal: wrapFor(in, nil)(j), kind: kind}
			var stderr bytes.Buffer
			r := &runner{in: Input{Config: in.Config, ConfigPath: in.ConfigPath, Boot: boot, Journal: faulty, Machine: seams, Stderr: &stderr}, fold: newFold(), tuner: tuner.New()}
			stop, runErr := r.run(context.Background())
			err = errors.Join(runErr, r.close(true, &stop))
			type observation struct {
				JournalFailed, BackendLive, ContainmentLatched, JournalError, ContainmentError bool
				LiveWrites                                                                     int
			}
			got := observation{faulty.failed, live, r.containmentFailed, errors.Is(err, io.ErrClosedPipe), errors.Is(err, machine.ErrContainment), smu.writes}
			want := observation{true, true, true, true, true, 0}
			if diff := cmp.Diff(want, got); diff != "" {
				t.Errorf("failed journal and teardown (-want +got):\n%s", diff)
			}
			t.Logf("live SMU writes=%d; containment latched=%v; error=%v", smu.writes, r.containmentFailed, err)
		})
	}
}

func TestBaselineReadFailureDeadEndsBeforeAnyWrite(t *testing.T) {
	t.Parallel()
	in := simInput(t.TempDir(), newSim(t, small()))
	failure := errors.New("baseline read unavailable")
	fired := false
	in.Machine.InterruptSMU(func(sim.SMUOperation) error {
		if !fired {
			fired = true
			return failure
		}
		return nil
	})
	stop := simulate(t, in)
	if stop.Reason != StopDeadEnd || stop.DeadEnd.Condition != journal.DeadEndSMU || !fired {
		t.Fatalf("failed baseline read: %+v, fired %t", stop, fired)
	}
	events := readEvents(t, in.Dir)
	var faults []*journal.SMUError
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.SMUError:
			faults = append(faults, p)
		case *journal.SMUIntent, *journal.TrialIntent, *journal.SessionBaseline, *journal.ProfileRestored:
			t.Fatalf("failed baseline allowed writes or claimed a baseline: %+v", e)
		}
	}
	if diff := cmp.Diff([]*journal.SMUError{{Op: journal.SMURead, Core: new(0), Error: failure.Error()}}, faults); diff != "" {
		t.Fatalf("operator-visible baseline failure (-want +got):\n%s", diff)
	}
	in.Machine.InterruptSMU(nil)
	if diff := cmp.Diff(small().BIOS, actualOffsets(t, in)); diff != "" {
		t.Fatalf("baseline failure changed offsets:\n%s", diff)
	}
}

func TestRestoreSMUFailureChangesOnlyCleanStopOutcome(t *testing.T) {
	t.Parallel()
	for _, alreadyDead := range []bool{false, true} {
		t.Run(map[bool]string{false: "signal", true: "backend evidence dead end"}[alreadyDead], func(t *testing.T) {
			t.Parallel()
			r, m, closeJournal := checkedRunner(t, []int{0, 0})
			defer closeJournal()
			if err := r.apply([]int{-5, -5}, &journal.ProfileApplied{Offsets: []int{-5, -5}, Condition: machine.Resident}, 0); err != nil {
				t.Fatal(err)
			}
			stop := Stop{Reason: StopSignal}
			r.shutdownEvent = &journal.Shutdown{Reason: journal.ShutdownSignal}
			if alreadyDead {
				dead, err := r.deadEnd(&journal.DeadEnd{Condition: journal.DeadEndNoEvidence, Detail: "backend mprime exhausted retries"})
				if err != nil {
					t.Fatal(err)
				}
				stop = *dead
			}
			original := stop
			m.FailWrite()
			if err := r.close(true, &stop); err != nil {
				t.Fatal(err)
			}
			want := journal.DeadEndSMU
			if alreadyDead {
				want = journal.DeadEndNoEvidence
			}
			if stop.Reason != StopDeadEnd || stop.DeadEnd == nil || stop.DeadEnd.Condition != want {
				t.Fatalf("restoration overwrote the wrong outcome: %+v, want %s", stop, want)
			}
			if alreadyDead {
				if diff := cmp.Diff(original, stop); diff != "" {
					t.Fatalf("restoration replaced the original dead end (-want +got):\n%s", diff)
				}
			}
			events := r.in.Journal.Events()
			failed := false
			for _, e := range events {
				if e.Kind == journal.KindSMUError {
					failed = true
				}
				if failed && (e.Kind == journal.KindSMUIntent || e.Kind == journal.KindProfileRestored) {
					t.Fatalf("failed restoration continued or claimed success: %+v", e)
				}
			}
			if !failed {
				t.Fatal("restoration failure was not recorded")
			}
			for core := range 2 {
				if offset, err := m.Seams().SMU.Offset(core); err != nil || offset != -5 {
					t.Fatalf("failed restore changed core %d: %d, %v", core, offset, err)
				}
			}
		})
	}
}
