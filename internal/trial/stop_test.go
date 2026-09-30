package trial

import (
	"context"
	"testing"
	"testing/synctest"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func TestStopJoinsWithoutWait(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		host := &fakeHost{}
		runner := New(fakeOptions(t, "work"))
		runner.host = host
		active, err := runner.Start(context.Background(), testSpec("stop", machine.R1, time.Hour))
		if err != nil {
			t.Fatal(err)
		}
		running := active.(*running)
		if err := running.Stop(); err != nil {
			t.Fatal(err)
		}
		host.mu.Lock()
		signals := append([]fakeSignal(nil), host.signals...)
		for _, p := range host.procs {
			select {
			case <-p.exited:
			default:
				t.Error("Stop returned before process exit")
			}
		}
		host.mu.Unlock()
		if err := running.Stop(); err != nil {
			t.Fatal(err)
		}
		host.mu.Lock()
		defer host.mu.Unlock()
		if diff := cmp.Diff(signals, host.signals, cmp.AllowUnexported(fakeSignal{})); diff != "" {
			t.Fatalf("repeated Stop sent more signals (-want +got):\n%s", diff)
		}
	})
}
