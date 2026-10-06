package sim

import (
	"context"
	"testing"
	"time"

	"github.com/shgew/togi/internal/machine"
)

func runSpec(t *testing.T, m *Machine, id string, regime machine.Regime, workload machine.Workload, cores []int, duration time.Duration, report machine.Reporter) (machine.Result, error) {
	t.Helper()
	run, err := m.Seams().Trials.Start(context.Background(), machine.TrialSpec{ID: id, Regime: regime, Workload: workload, Condition: machine.Together, Cores: cores, Duration: duration})
	if err != nil {
		t.Fatalf("start %s: %v", id, err)
	}
	return run.Wait(context.Background(), report)
}

type signalRecorder struct{ signals []machine.Signal }

func (*signalRecorder) Progress(string)       {}
func (*signalRecorder) Sample(machine.Sample) {}
func (r *signalRecorder) Signal(_ int, signal machine.Signal, _ string) {
	r.signals = append(r.signals, signal)
}
