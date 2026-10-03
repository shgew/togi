//go:build integration && linux

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
)

func hungCommand(t *testing.T, name string) string {
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

func commandGone(t *testing.T, marker string) {
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
	p := scopeProcess{PID: pid, Start: start}
	alive, err := (osHost{}).ProcessAlive(p)
	if err != nil || alive {
		t.Fatalf("command survived cleanup: %+v alive=%t err=%v", p, alive, err)
	}
}

func TestProcessScopeSystemctlDeadline(t *testing.T) {
	marker := hungCommand(t, "systemctl")
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
	commandGone(t, marker)
	t.Logf("real PATH-local systemctl timed out after %s; missing-scope text did not confirm containment", elapsed)
}

func TestProcessScopeJournalctlDeadline(t *testing.T) {
	marker := hungCommand(t, "journalctl")
	started := time.Now()
	read, err := detect.NewKernel(nil).ReadMCEs("00000000000000000000000000000000", "")
	elapsed := time.Since(started)
	if !errors.Is(err, context.DeadlineExceeded) || read.Cursor != "" || len(read.MCEs) != 0 {
		t.Fatalf("hung real journalctl became a clean kernel observation: read=%+v err=%v", read, err)
	}
	if elapsed < 30*time.Second || elapsed > 35*time.Second {
		t.Fatalf("journalctl did not use its full production deadline: %s", elapsed)
	}
	commandGone(t, marker)
	t.Logf("public detector rejected real hung journalctl after its full 30s deadline (%s)", elapsed)
}
