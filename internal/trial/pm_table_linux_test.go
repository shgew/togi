package trial

import (
	"context"
	"errors"
	"path/filepath"
	"sync/atomic"
	"syscall"
	"testing"
	"testing/synctest"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/smu"
)

func TestBlockedPMTablePreservesSupervision(t *testing.T) {
	for _, ending := range []string{"deadline", "cancellation", "backend error"} {
		t.Run(ending, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				release := make(chan struct{})
				defer close(release)
				cores := make([]machine.CoreInfo, 16)
				for i := range cores {
					cores[i] = machine.CoreInfo{Core: i, CCD: i / 8}
				}
				var transfers atomic.Int64
				pmTable := smu.NewPMTableReader("/", cores, func(path string) ([]byte, error) {
					if filepath.Base(path) == "pm_table_version" {
						return []byte{0x05, 0x02, 0x62, 0x00}, nil
					}
					transfers.Add(1)
					<-release
					return make([]byte, 2452), nil
				})
				synctest.Wait()
				o := fakeOptions(t, "work")
				o.SampleInterval, o.PMTable = 50*time.Millisecond, pmTable
				r := New(o)
				h := &fakeHost{}
				r.host = h
				regime := machine.R1
				if ending == "deadline" {
					regime = machine.R6
				}
				run, err := r.Start(context.Background(), testSpec("blocked-table", regime, 600*time.Millisecond))
				if err != nil {
					t.Fatal(err)
				}
				f := &fakeSampleFile{}
				run.(*running).openSamples = func(string) (sampleFile, error) { return f, nil }
				ctx, cancel := context.WithCancel(context.Background())
				defer cancel()
				done := make(chan struct{})
				var result machine.Result
				go func() {
					result, err = run.Wait(ctx, &recorder{})
					close(done)
				}()
				wantRan := 600 * time.Millisecond
				if ending != "deadline" {
					time.Sleep(150 * time.Millisecond)
					if ending == "cancellation" {
						cancel()
					} else {
						run.(*running).events <- streamEvent{index: 0, line: "COMPUTE ERROR"}
					}
					wantRan = 150 * time.Millisecond
				}
				time.Sleep(700 * time.Millisecond)
				synctest.Wait()
				select {
				case <-done:
				default:
					t.Fatal("trial supervision waited for the blocked SMU transfer")
				}
				if diff := cmp.Diff(wantRan, result.Ran); diff != "" {
					t.Error(diff)
				}
				if ending == "cancellation" {
					if !errors.Is(err, context.Canceled) {
						t.Errorf("cancellation lost: %v", err)
					}
				} else if err != nil {
					t.Error(err)
				}
				if ending == "backend error" && result.Signal != machine.ComputationError {
					t.Errorf("backend evidence delayed or lost: %+v", result)
				}
				if ending == "deadline" {
					want := []fakeSignal{
						{1000, syscall.SIGSTOP},
						{1000, syscall.SIGCONT},
						{1000, syscall.SIGSTOP},
						{1000, syscall.SIGCONT},
						{1000, syscall.SIGTERM},
						{1000, syscall.SIGKILL},
					}
					if diff := cmp.Diff(want, h.recordedSignals(), cmp.AllowUnexported(fakeSignal{})); diff != "" {
						t.Error(diff)
					}
				}
				if transfers.Load() != 1 {
					t.Error("blocked sampling spawned replacement transfers")
				}
				for _, sample := range f.samples {
					if sample.PMTable != nil {
						t.Errorf("blocked read persisted lanes at %dms", sample.ElapsedMS)
					}
				}
			})
		})
	}
}
