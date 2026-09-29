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
	passedErr  error
	cancel     context.CancelFunc
	smu        machine.SMU
	tuned      bool
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
func (t *ownerTrials) Passed(id string) error {
	if t.cancel != nil {
		t.cancel()
	}
	return t.passedErr
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
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	_, err := r.Wait(ctx, cleanupReport{})
	if errors.Is(err, context.Canceled) {
		return nil
	}
	return err
}

type ownerJournal struct {
	Journal
	fail  bool
	cause error
}

func (j ownerJournal) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	if j.fail && p.Kind() == journal.KindTrialStart {
		return journal.Event{}, j.cause
	}
	return j.Journal.Append(p, cause...)
}
func TestRunOwnerEveryExit(t *testing.T) {
	for _, mode := range []string{"passed warning", "ordinary error", "panic"} {
		t.Run(mode, func(t *testing.T) {
			t.Parallel()
			r, m, closeJournal := checkedRunner(t, []int{0, 0})
			r.in.Config.StartOffsets = map[int]int{0: -5, 1: -5}
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			original := errors.New("injected failure")
			cleanupErr := errors.New("injected close failure")
			trials := &ownerTrials{Trials: r.in.Machine.Trials, panicStart: mode == "panic", passedErr: original, cancel: cancel}
			trials.smu = r.in.Machine.SMU
			r.in.Machine.Trials = trials
			r.in.Machine.SMU = ownerSMU{SMU: r.in.Machine.SMU, trials: trials, t: t}
			r.in.Journal = ownerJournal{Journal: r.in.Journal, fail: mode == "ordinary error", cause: original}
			r.in.Rotations = 1
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
				if mode != "passed warning" {
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
				var warning *journal.SessionWarning
				var pass int
				for _, e := range r.in.Journal.Events() {
					if p, ok := e.Data.(*journal.TrialEnd); ok && p.Outcome == journal.OutcomePass {
						pass = e.Seq
					}
					if p, ok := e.Data.(*journal.SessionWarning); ok {
						warning = p
						if diff := cmp.Diff([]int{pass}, e.Cause); diff != "" {
							t.Fatal(diff)
						}
					}
				}
				if warning == nil {
					t.Fatal("missing warning")
				}
				if diff := cmp.Diff(&journal.SessionWarning{Operation: "retain passed trial", Trial: "0001", Error: original.Error()}, warning); diff != "" {
					t.Fatal(diff)
				}
			}
		})
	}
}
