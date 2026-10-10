package session

import (
	"context"
	"errors"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

type ownerSMU struct {
	machine.SMU
	trials *ownerTrials
	t      *testing.T
}

func (s ownerSMU) SetAllOffsets(offset int) error {
	if s.trials.active {
		s.t.Error("offsets written before workload joined")
	}
	return s.SMU.SetAllOffsets(offset)
}

func (s ownerSMU) SetOffset(core, offset int) error {
	if s.trials.active {
		s.t.Error("offsets written before workload joined")
	}
	return s.SMU.SetOffset(core, offset)
}

type ownerTrials struct {
	machine.Trials
	active     bool
	stopped    int
	panicStart bool
	stopErr    error
	endedErr   error
	smu        machine.SMU
	tuned      bool
	t          *testing.T
	journal    Journal
	ended      []string
}

func (t *ownerTrials) Start(ctx context.Context, spec machine.TrialSpec) (machine.Running, error) {
	r, err := t.Trials.Start(ctx, spec)
	if err != nil {
		return nil, err
	}
	t.active = true
	offset, _ := t.smu.Offset(spec.Cores[0])
	t.tuned = offset < 0
	return &ownerRunning{Running: r, owner: t}, nil
}

// Ended stands for compressing a trial's files: the backend must be joined first and the durable trial.end must follow.
func (t *ownerTrials) Ended(id string) error {
	if t.active {
		t.t.Errorf("trial %s files finalized before its workload joined", id)
	}
	for _, e := range t.journal.Events() {
		if p, ok := e.Data.(*journal.TrialEnd); ok && p.Trial == id {
			t.t.Errorf("trial %s files finalized after its durable trial.end at seq %d", id, e.Seq)
		}
	}
	t.ended = append(t.ended, id)
	if t.endedErr != nil {
		return t.endedErr
	}
	return t.Trials.Ended(id)
}

type ownerRunning struct {
	machine.Running
	owner *ownerTrials
}

func (r *ownerRunning) Started() machine.Started {
	if r.owner.panicStart {
		panic("owner panic")
	}
	return r.Running.Started()
}
func (r *ownerRunning) Wait(ctx context.Context, report machine.Reporter) (machine.Result, error) {
	result, err := r.Running.Wait(ctx, report)
	r.owner.active = false
	r.owner.stopped++
	return result, err
}
func (r *ownerRunning) Stop() error {
	if r.owner.stopErr != nil {
		return r.owner.stopErr
	}
	err := r.Running.Stop()
	r.owner.active = false
	r.owner.stopped++
	return err
}

type ownerJournal struct {
	Journal
	fail         bool
	cause        error
	cancelOnPass context.CancelFunc
}

func (j ownerJournal) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	if j.fail && p.Kind() == journal.KindTrialStart {
		return journal.Event{}, j.cause
	}
	event, err := j.Journal.Append(p, cause...)
	if end, ok := p.(*journal.TrialEnd); ok && err == nil && end.Outcome == journal.OutcomePass && j.cancelOnPass != nil {
		j.cancelOnPass()
	}
	return event, err
}
func TestRunOwnerEveryExit(t *testing.T) {
	for _, mode := range []string{"passed trial", "compression warning", "ordinary error", "panic"} {
		t.Run(mode, func(t *testing.T) {
			t.Parallel()
			r, m, closeJournal := checkedRunner(t, []int{0, 0})
			r.in.Config.StartOffsets = map[int]int{0: -5, 1: -5}
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			original := errors.New("injected failure")
			cleanupErr := errors.New("injected close failure")
			trials := &ownerTrials{Trials: r.in.Machine.Trials, panicStart: mode == "panic", t: t}
			if mode == "compression warning" {
				trials.endedErr = original
			}
			trials.smu = r.in.Machine.SMU
			r.in.Machine.Trials = trials
			r.in.Machine.SMU = ownerSMU{SMU: r.in.Machine.SMU, trials: trials, t: t}
			r.in.Journal = ownerJournal{Journal: r.in.Journal, fail: mode == "ordinary error", cause: original, cancelOnPass: cancel}
			trials.journal = r.in.Journal
			r.in.Cycles = 1
			closed := false
			r.in.Close = func() error {
				closed = true
				if trials.active {
					t.Error("journal closed before workload joined")
				}
				var offsets []int
				for core := range 2 {
					o, err := m.Seams().SMU.Offset(core)
					if err != nil {
						t.Fatal(err)
					}
					offsets = append(offsets, o)
				}
				if diff := cmp.Diff([]int{0, 0}, offsets); diff != "" {
					t.Errorf("cleanup offsets (-want +got):\n%s", diff)
				}
				closeJournal()
				if mode != "passed trial" && mode != "compression warning" {
					return cleanupErr
				}
				return nil
			}
			var runErr error
			var recovered any
			func() { defer func() { recovered = recover() }(); _, runErr = Run(ctx, r.in) }()
			if !closed || trials.active || trials.stopped != 1 || !trials.tuned {
				t.Fatalf("closed=%v active=%v joins=%d tuned=%v", closed, trials.active, trials.stopped, trials.tuned)
			}
			wantEnded := []string(nil)
			if mode == "passed trial" || mode == "compression warning" {
				wantEnded = []string{"0001"}
			}
			if diff := cmp.Diff(wantEnded, trials.ended); diff != "" {
				t.Fatalf("finalized trials (-want +got):\n%s", diff)
			}
			switch mode {
			case "panic":
				if diff := cmp.Diff(any("owner panic"), recovered); diff != "" {
					t.Fatal(diff)
				}
			case "ordinary error":
				if !errors.Is(runErr, original) || !errors.Is(runErr, cleanupErr) {
					t.Fatalf("original or cleanup error lost: %v", runErr)
				}
			default:
				if runErr != nil {
					t.Fatal(runErr)
				}
				var warnings int
				for _, e := range r.in.Journal.Events() {
					if e.Kind != journal.KindSessionWarning {
						continue
					}
					warnings++
					if mode != "compression warning" {
						t.Fatalf("passed trial left a warning: %+v", e.Data)
					}
					want := &journal.SessionWarning{Operation: "compress trial files", Trial: "0001", Error: original.Error()}
					if diff := cmp.Diff(want, e.Data); diff != "" {
						t.Fatal(diff)
					}
					if len(e.Cause) != 1 {
						t.Fatalf("warning cause = %v", e.Cause)
					}
					caused := r.in.Journal.Events()[e.Cause[0]-1].Data.(*journal.TrialEnd)
					if caused.Outcome != journal.OutcomePass {
						t.Fatalf("compression failure changed trial outcome: %+v", caused)
					}
				}
				if mode == "compression warning" && warnings != 1 {
					t.Fatalf("compression warnings = %d, want 1", warnings)
				}
			}
		})
	}
}

func TestRunOwnerSkipsRestoreUntilWorkloadJoined(t *testing.T) {
	t.Parallel()
	r, m, closeJournal := checkedRunner(t, []int{0, 0})
	for core := range 2 {
		if err := m.Seams().SMU.SetOffset(core, -5); err != nil {
			t.Fatal(err)
		}
	}
	r.applied = []int{-5, -5}
	stopErr := errors.New("workload did not exit after SIGKILL")
	trials := &ownerTrials{active: true, stopErr: stopErr}
	r.in.Machine.SMU = ownerSMU{SMU: r.in.Machine.SMU, trials: trials, t: t}
	r.running = &ownerRunning{owner: trials}
	r.shutdownEvent = &journal.Shutdown{Reason: "stopped by signal"}
	closed := false
	r.in.Close = func() error {
		closed = true
		closeJournal()
		return nil
	}
	stop := Stop{Reason: StopSignal}
	if err := r.close(true, &stop); !errors.Is(err, stopErr) {
		t.Fatalf("cleanup error lost: %v", err)
	}
	var actual []int
	for core := range 2 {
		offset, err := m.Seams().SMU.Offset(core)
		if err != nil {
			t.Fatal(err)
		}
		actual = append(actual, offset)
	}
	if diff := cmp.Diff([]int{-5, -5}, actual); diff != "" {
		t.Fatalf("offsets changed while workload unjoined (-want +got):\n%s", diff)
	}
	for _, e := range r.in.Journal.Events() {
		if e.Kind == journal.KindShutdown || e.Kind == journal.KindProfileRestored || e.Kind == journal.KindSMUIntent {
			t.Fatalf("cleanup event recorded while workload unjoined: %s", e.Kind)
		}
	}
	if !closed || !trials.active {
		t.Fatalf("closed=%v active=%v", closed, trials.active)
	}
}
