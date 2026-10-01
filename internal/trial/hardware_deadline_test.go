//go:build hardware && linux

package trial

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/shgew/togi/internal/detect"
	"github.com/shgew/togi/internal/machine"
)

func hardwareHungCommand(t *testing.T, name string) string {
	t.Helper()
	dir := t.TempDir()
	marker := filepath.Join(dir, "command.pid")
	script := "#!/bin/sh\nexec " + strconv.Quote(stageHelper(t)) + " -test.run=TestHelperProcess -- --helper hung-" + name + "\n"
	if err := os.WriteFile(filepath.Join(dir, name), []byte(script), 0755); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("TOGI_HELPER_COMMAND_PID", marker)
	return marker
}

func hardwareCommandGone(t *testing.T, marker string) {
	t.Helper()
	data, err := os.ReadFile(marker)
	if err != nil {
		t.Fatalf("hung executable never reached its blocking operation: %v", err)
	}
	fields := strings.Fields(string(data))
	if len(fields) != 2 {
		t.Fatalf("invalid command identity %q", data)
	}
	pid, err := strconv.Atoi(fields[0])
	if err != nil {
		t.Fatal(err)
	}
	start, err := strconv.ParseInt(fields[1], 10, 64)
	if err != nil {
		t.Fatal(err)
	}
	hardwareProcessesGone(t, scopeProcess{PID: pid, Start: start})
}

func TestHardwareScopeSystemctlDeadline(t *testing.T) {
	_ = hardwareRoot(t)
	marker := hardwareHungCommand(t, "systemctl")
	started := time.Now()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	out, err := newOSHost().KillScope(ctx, "togi-trial-hung")
	elapsed := time.Since(started)
	if !errors.Is(err, context.DeadlineExceeded) || scopeMissing(out, err) {
		t.Fatalf("timed-out real systemctl was accepted as missing scope: output=%q err=%v", out, err)
	}
	if !strings.Contains(string(out), "not loaded") {
		t.Fatalf("missing-scope diagnostic was not emitted by hung executable: %q", out)
	}
	if elapsed < 2*time.Second || elapsed > 3*time.Second {
		t.Fatalf("systemctl ignored its remaining context deadline: %s", elapsed)
	}
	hardwareCommandGone(t, marker)
	t.Logf("real PATH-local systemctl timed out after %s; missing-scope text did not confirm containment", elapsed)
}

func TestHardwareScopeJournalctlDeadline(t *testing.T) {
	_ = hardwareRoot(t)
	marker := hardwareHungCommand(t, "journalctl")
	started := time.Now()
	read, err := detect.NewKernel(nil).ReadMCEs("00000000000000000000000000000000", "")
	elapsed := time.Since(started)
	if !errors.Is(err, context.DeadlineExceeded) || read.Cursor != "" || len(read.MCEs) != 0 {
		t.Fatalf("hung real journalctl became a clean kernel observation: read=%+v err=%v", read, err)
	}
	if elapsed < 30*time.Second || elapsed > 35*time.Second {
		t.Fatalf("journalctl did not use its full production deadline: %s", elapsed)
	}
	hardwareCommandGone(t, marker)
	t.Logf("public detector rejected real hung journalctl after its full 30s deadline (%s)", elapsed)
}

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
