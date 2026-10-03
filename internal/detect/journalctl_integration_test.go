//go:build integration && linux

package detect

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strconv"
	"syscall"
	"testing"
	"time"
)

func TestHungJournalctlHelper(t *testing.T) {
	marker := os.Getenv("TOGI_HUNG_JOURNALCTL_PID")
	if marker == "" {
		t.Skip("helper process for TestJournalctlKilledAtDeadline")
	}
	if err := os.WriteFile(marker, []byte(strconv.Itoa(os.Getpid())), 0644); err != nil {
		os.Exit(2)
	}
	time.Sleep(time.Hour)
}

func TestJournalctlKilledAtDeadline(t *testing.T) {
	executable, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	marker := filepath.Join(dir, "journalctl.pid")
	script := "#!/bin/sh\nexec " + strconv.Quote(executable) + " -test.run=^TestHungJournalctlHelper$\n"
	if err := os.WriteFile(filepath.Join(dir, "journalctl"), []byte(script), 0755); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", dir+string(os.PathListSeparator)+os.Getenv("PATH"))
	t.Setenv("TOGI_HUNG_JOURNALCTL_PID", marker)
	started := time.Now()
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	_, _, _, err = journalctlOutput(ctx, nil)
	elapsed := time.Since(started)
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("hung journalctl = %v, want its deadline", err)
	}
	if elapsed > 2*time.Second {
		t.Fatalf("hung journalctl outlived its deadline: %s", elapsed)
	}
	data, err := os.ReadFile(marker)
	if err != nil {
		t.Fatalf("hung journalctl never started: %v", err)
	}
	pid, err := strconv.Atoi(string(data))
	if err != nil {
		t.Fatal(err)
	}
	if err := syscall.Kill(pid, 0); !errors.Is(err, syscall.ESRCH) {
		t.Fatalf("hung journalctl %d survived its deadline: %v", pid, err)
	}
}
