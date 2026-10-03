//go:build hardware && linux

package trial

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/shgew/togi/internal/machine"
)

func TestHardwareScopeOversizedOutput(t *testing.T) {
	user := hardwareRoot(t)
	for _, mode := range []string{"oversized-stdout", "oversized-stderr", "watched-oversized"} {
		t.Run(mode, func(t *testing.T) {
			o := hardwareOptions(t, user, mode)
			spec := testSpec("hw-"+mode, machine.R1, time.Minute)
			spec.CPUs = o.Cores[0].CPUs
			scope := "togi-trial-" + spec.ID
			hardwareScopeCleanup(t, scope)
			started, err := New(o).Start(context.Background(), spec)
			if err != nil {
				t.Fatal(err)
			}
			trial := started.(*running)
			t.Cleanup(func() { _ = trial.Stop() })
			ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			defer cancel()
			trial.awaitScope(ctx, trial.instances[0])
			if !trial.host.InScope(trial.instances[0].PID, scope) {
				t.Fatal("oversized writer never entered its real scope")
			}
			begin := time.Now()
			result, err := trial.Wait(context.Background(), &recorder{})
			elapsed := time.Since(begin)
			if !errors.Is(err, errOutputLineTooLong) || errors.Is(err, machine.ErrContainment) || result.Inconclusive == "" || result.Signal != "" || len(result.Escaped) != 0 {
				t.Fatalf("scoped oversized result=%+v err=%v", result, err)
			}
			if elapsed > teardownLimit {
				t.Fatalf("oversized scope cleanup exceeded shared deadline: %s", elapsed)
			}
			inst := trial.instances[0]
			select {
			case <-inst.joined:
			default:
				t.Fatal("scoped launcher or output reader remains")
			}
			p := inst.process.(*execProcess)
			hardwareProcessesGone(t, scopeProcess{PID: p.PID(), Start: p.start})
			for _, reader := range []*os.File{p.stdout, p.stderr} {
				if _, err := reader.Stat(); !errors.Is(err, os.ErrClosed) {
					t.Fatalf("scoped output reader remains open: %v", err)
				}
			}
			var prefix []byte
			if mode == "watched-oversized" {
				prefix = inst.watch[0].lines.pending
			} else {
				name := strings.TrimPrefix(mode, "oversized-") + ".log"
				prefix, err = os.ReadFile(filepath.Join(o.Dir, spec.ID, "work", name))
				if err != nil {
					t.Fatal(err)
				}
			}
			if string(prefix) != strings.Repeat("x", outputLineLimit) {
				t.Fatalf("retained diagnostic prefix is not exactly the first %d bytes: size=%d", outputLineLimit, len(prefix))
			}
			hardwareScopesGone(t, scope)
			t.Logf("%s: real scope cleaned in %s; inconclusive line-limit diagnostic and exact 64KiB prefix retained; readers joined", mode, elapsed)
		})
	}
}
