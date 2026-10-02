package simrun

import (
	"bytes"
	"context"
	"os"
	"path/filepath"
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
)

type interruptionMatrix struct {
	name        string
	cfg         sim.Config
	config      config.Config
	prefix      []journal.Event
	closeWindow func(journal.Event, *bool) bool
}

func (matrix interruptionMatrix) machine(t *testing.T, dir string) *sim.Machine {
	t.Helper()
	lines := make([][]byte, len(matrix.prefix))
	var boots []string
	reasons := map[string]machine.ResetKind{}
	for i, e := range matrix.prefix {
		lines[i] = e.Raw
		if !slices.Contains(boots, e.Boot) {
			boots = append(boots, e.Boot)
		}
		if p, ok := e.Data.(*journal.CrashDetected); ok {
			reasons[e.Boot] = p.ResetReason
		}
	}
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), append(bytes.Join(lines, []byte{'\n'}), '\n'), 0o600); err != nil {
		t.Fatal(err)
	}
	resumed, err := sim.Resume(dir, matrix.cfg)
	if err != nil {
		t.Fatal(err)
	}
	resumed.Boots = 0
	m, err := sim.New(resumed)
	if err != nil {
		t.Fatal(err)
	}
	for i := range boots {
		kind := machine.ResetPowerLoss
		if i+1 < len(boots) {
			kind = reasons[boots[i+1]]
		}
		m.NextReset(kind)
		m.Reboot()
	}
	return m
}

func (matrix interruptionMatrix) run(t *testing.T, at, effectAt int) ([]journal.Event, []sim.SMUOperation) {
	t.Helper()
	dir := t.TempDir()
	m := matrix.machine(t, dir)
	closing, closed, crashed, appended := false, false, false, 0
	var accesses []sim.SMUOperation
	m.InterruptSMU(func(access sim.SMUOperation) error {
		accesses = append(accesses, access)
		if !crashed && len(accesses) == effectAt {
			crashed = true
			m.NextReset(machine.ResetPowerLoss)
			m.Crash()
			return machine.ErrCrashed
		}
		return nil
	})
	in := Input{Config: matrix.config, ConfigPath: config.DefaultPath, Dir: dir, Machine: m,
		Until: func(e journal.Event) bool {
			if closed {
				return at < 0 || crashed
			}
			closed = matrix.closeWindow(e, &closing)
			return closed && at < 0
		},
	}
	if at >= 0 {
		in.Wrap = func(j session.Journal) session.Journal {
			if crashed {
				return j
			}
			if at == 0 {
				crashed = true
				m.NextReset(machine.ResetPowerLoss)
				m.Crash()
				return j
			}
			return &matrixCrash{Journal: j, machine: m, at: at, count: &appended, crashed: &crashed}
		}
	}
	stop, err := Simulate(context.Background(), in)
	if err != nil {
		t.Fatalf("%s append %d SMU %d: %v", matrix.name, at, effectAt, err)
	}
	if stop.Reason != session.StopSignal || !closed {
		t.Fatalf("%s append %d SMU %d: stop %+v deadend %+v, closed %v", matrix.name, at, effectAt, stop, stop.DeadEnd, closed)
	}
	if (at >= 0 || effectAt > 0) && !crashed {
		t.Fatalf("%s append %d SMU %d did not interrupt", matrix.name, at, effectAt)
	}
	events, torn, err := journal.Read(dir)
	if err != nil || torn != nil {
		t.Fatalf("%s append %d SMU %d: reopen %v, torn %q", matrix.name, at, effectAt, err, torn)
	}
	return events, accesses
}

func assertMatrixDecisions(t *testing.T, want matrixResult, events []journal.Event) {
	t.Helper()
	if diff := cmp.Diff(want, matrixCommitments(events)); diff != "" {
		t.Fatalf("resumed decisions (-uninterrupted +resumed):\n%s", diff)
	}
	assertMatrixRerunRetries(t, events)
	seen := map[[2]int]bool{}
	for _, e := range events {
		if p, ok := e.Data.(*journal.HuntMask); ok {
			key := [2]int{p.Hunt, p.Mask}
			if seen[key] {
				t.Fatalf("duplicated hunt.mask %v", key)
			}
			seen[key] = true
		}
	}
}

func assertMatrixRerunRetries(t *testing.T, events []journal.Event) {
	t.Helper()
	intents := map[string]journal.Event{}
	pending := map[int]journal.Event{}
	completed := map[string]bool{}
	normalize := func(p journal.TrialIntent) journal.TrialIntent {
		p.Trial, p.KernelBoundary, p.Retry = "", journal.KernelBoundary{}, false
		return p
	}
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.TrialIntent:
			if p.Rerun {
				intents[p.Trial] = e
			}
		case *journal.TrialEnd:
			intentEvent, ok := intents[p.Trial]
			if !ok {
				continue
			}
			if len(intentEvent.Cause) == 0 {
				t.Fatalf("rerun %s has no obligation cause", p.Trial)
			}
			obligation := intentEvent.Cause[0]
			if p.Outcome == journal.OutcomeInconclusive {
				pending[obligation] = intentEvent
				continue
			}
			if completed[p.Trial] {
				t.Fatalf("rerun %s completed twice", p.Trial)
			}
			completed[p.Trial] = true
			intent := intentEvent.Data.(*journal.TrialIntent)
			if intent.Retry {
				previous, ok := pending[obligation]
				if !ok || !slices.Equal(previous.Cause, intentEvent.Cause) || !cmp.Equal(normalize(*previous.Data.(*journal.TrialIntent)), normalize(*intent)) {
					t.Fatalf("rerun retry %s changed its interrupted obligation", p.Trial)
				}
			}
			delete(pending, obligation)
		}
	}
}
