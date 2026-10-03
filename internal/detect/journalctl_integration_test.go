//go:build integration && linux

package detect

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"
)

func journalctlProcessStart(pid int) (string, error) {
	data, err := os.ReadFile(fmt.Sprintf("/proc/%d/stat", pid))
	if err != nil {
		return "", err
	}
	end := strings.LastIndexByte(string(data), ')')
	if end < 0 {
		return "", fmt.Errorf("malformed process stat for %d: %q", pid, data)
	}
	fields := strings.Fields(string(data[end+1:]))
	if len(fields) < 20 {
		return "", fmt.Errorf("short process stat for %d: %q", pid, data)
	}
	return fields[19], nil
}

func TestHungJournalctlHelper(t *testing.T) {
	marker := os.Getenv("TOGI_HUNG_JOURNALCTL_PID")
	if marker == "" {
		t.Skip("helper process for TestJournalctlKilledAtDeadline")
	}
	start, err := journalctlProcessStart(os.Getpid())
	if err != nil {
		os.Exit(2)
	}
	if err := os.WriteFile(marker, []byte(strconv.Itoa(os.Getpid())+" "+start), 0644); err != nil {
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
	fields := strings.Fields(string(data))
	if len(fields) != 2 {
		t.Fatalf("invalid hung journalctl identity %q", data)
	}
	pid, err := strconv.Atoi(fields[0])
	if err != nil {
		t.Fatal(err)
	}
	start, err := journalctlProcessStart(pid)
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		t.Fatal(err)
	}
	if err == nil && start == fields[1] {
		t.Fatalf("hung journalctl %d survived its deadline", pid)
	}
}
