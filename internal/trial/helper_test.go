//go:build (integration || hardware) && linux

package trial

import (
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"
	"unsafe"

	"github.com/shgew/togi/internal/backend"
	"github.com/shgew/togi/internal/machine"
)

type helperBackend struct {
	mode       string
	executable string
}

func (h helperBackend) Name() string           { return "helper" }
func (h helperBackend) Check() (string, error) { return "ok", nil }
func (h helperBackend) Prepare(_ machine.Workload, _ string, cpus []int) (backend.Launch, error) {
	executable := h.executable
	if executable == "" {
		executable = os.Args[0]
	}
	launch := backend.Launch{Argv: []string{"env", "GORACE=atexit_sleep_ms=0", "taskset", "-c", strconv.Itoa(cpus[0]), executable, "-test.run=TestHelperProcess", "--", "--helper", h.mode}}
	if strings.HasPrefix(h.mode, "watched") {
		launch.Watch = []string{"results.txt"}
	}
	return launch, nil
}
func (h helperBackend) Classify(line string) backend.Line { return classifyHelper(line) }

func stageHelper(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	t.Cleanup(func() {
		if err := os.Chmod(dir, 0755); err != nil {
			t.Error(err)
		}
	})
	if err := os.Chmod(filepath.Dir(dir), 0755); err != nil {
		t.Fatal(err)
	}
	source, err := os.Open(os.Args[0])
	if err != nil {
		t.Fatal(err)
	}
	defer source.Close()
	path := filepath.Join(dir, "helper")
	target, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_EXCL, 0555)
	if err != nil {
		t.Fatal(err)
	}
	defer target.Close()
	if _, err := io.Copy(target, source); err != nil {
		t.Fatal(err)
	}
	if err := target.Close(); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(dir, 0555); err != nil {
		t.Fatal(err)
	}
	return path
}

func TestHelperProcess(t *testing.T) {
	idx := slices.Index(os.Args, "--helper")
	if idx < 0 {
		return
	}
	mode := os.Args[idx+1]
	if targetText, ok := strings.CutPrefix(mode, "escape:"); ok {
		runtime.LockOSThread()
		target, err := strconv.Atoi(targetText)
		if err != nil {
			os.Exit(2)
		}
		mask := make([]byte, target/8+1)
		mask[target/8] = 1 << uint(target%8)
		_, _, errno := syscall.RawSyscall(syscall.SYS_SCHED_SETAFFINITY, 0, uintptr(len(mask)), uintptr(unsafe.Pointer(&mask[0])))
		if errno != 0 {
			fmt.Println("PIN FAILED:", errno)
			os.Exit(0)
		}
		until := time.Now().Add(5 * time.Second)
		for time.Now().Before(until) {
		}
		os.Exit(0)
	}
	switch mode {
	case "exit":
		os.Exit(0)
	case "error":
		time.Sleep(300 * time.Millisecond)
		fmt.Println("COMPUTE ERROR")
		os.Exit(0)
	case "oversized-stdout", "oversized-stderr", "watched-oversized", "oversized-affinity", "oversized-computation":
		text := strings.Repeat("x", 1024*1024)
		switch mode {
		case "oversized-affinity":
			fmt.Fprintln(os.Stderr, "Failed to set core affinity to core: 42")
		case "oversized-computation":
			fmt.Fprintln(os.Stderr, "Checksum mismatch")
		}
		switch mode {
		case "oversized-stdout", "oversized-affinity", "oversized-computation":
			fmt.Print(text)
		case "oversized-stderr":
			fmt.Fprint(os.Stderr, text)
		case "watched-oversized":
			if err := os.WriteFile("results.txt", []byte(text), 0644); err != nil {
				os.Exit(2)
			}
		}
		time.Sleep(5 * time.Second)
		os.Exit(0)
	case "watched":
		if err := os.WriteFile("results.txt", []byte("COMPUTE ERROR\n"), 0644); err != nil {
			os.Exit(2)
		}
		os.Exit(0)
	case "watched-precedence":
		if err := os.WriteFile("results.txt", []byte("PIN FAILED\nCOMPUTE ERROR\nAFFINITY:42\n"), 0644); err != nil {
			os.Exit(2)
		}
		os.Exit(0)
	case "setup-then-error":
		fmt.Println("PIN FAILED")
		fmt.Println("COMPUTE ERROR")
		os.Exit(0)
	case "error-then-affinity":
		fmt.Println("COMPUTE ERROR")
		fmt.Println("AFFINITY:42")
		os.Exit(0)
	case "descendant", "pipe-descendant", "scope-owner":
		cmd := exec.Command(os.Args[0], "-test.run=TestHelperProcess", "--", "--helper", "orphan")
		if mode != "descendant" {
			cmd.Stdout, cmd.Stderr = os.Stdout, os.Stderr
		}
		cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true}
		if err := cmd.Start(); err != nil {
			os.Exit(2)
		}
		if err := os.WriteFile("descendant.pid", []byte(strconv.Itoa(cmd.Process.Pid)), 0644); err != nil {
			os.Exit(2)
		}
		if mode == "pipe-descendant" {
			fmt.Print("COMPUTE ERROR")
		}
		if mode == "scope-owner" {
			time.Sleep(time.Hour)
		}
		os.Exit(0)
	case "orphan":
		for {
			time.Sleep(time.Hour)
		}
	case "sleep":
		time.Sleep(5 * time.Second)
		os.Exit(0)
	case "work":
		deadline := time.Now().Add(5 * time.Second)
		for time.Now().Before(deadline) {
			until := time.Now().Add(100 * time.Millisecond)
			for time.Now().Before(until) {
			}
			fmt.Println("progress 1")
		}
		os.Exit(0)
	}
	os.Exit(2)
}

func testCPUs(t *testing.T) []int {
	t.Helper()
	b, err := os.ReadFile("/proc/self/status")
	if err != nil {
		t.Fatal(err)
	}
	for line := range strings.SplitSeq(string(b), "\n") {
		if text, ok := strings.CutPrefix(line, "Cpus_allowed_list:"); ok {
			ranges := strings.Split(strings.TrimSpace(text), ",")
			var cpus []int
			for _, part := range ranges {
				bounds := strings.SplitN(part, "-", 2)
				start, err := strconv.Atoi(bounds[0])
				if err != nil {
					t.Fatal(err)
				}
				end := start
				if len(bounds) == 2 {
					end, err = strconv.Atoi(bounds[1])
					if err != nil {
						t.Fatal(err)
					}
				}
				for cpu := start; cpu <= end && len(cpus) < 2; cpu++ {
					cpus = append(cpus, cpu)
				}
				if len(cpus) == 2 {
					break
				}
			}
			if len(cpus) < 2 {
				t.Skip("two CPUs needed")
			}
			return cpus
		}
	}
	t.Fatal("missing CPU affinity list")
	return nil
}
func testOptions(t *testing.T, mode string) Options {
	t.Helper()
	cpus := testCPUs(t)
	return Options{Dir: t.TempDir(), NoScope: true, Backends: map[machine.Backend]backend.Backend{machine.Mprime: helperBackend{mode: mode}}, SampleInterval: 50 * time.Millisecond, StallGrace: time.Hour, Cores: []machine.CoreInfo{{Core: 0, CCD: 0, CPUs: []int{cpus[0]}}, {Core: 1, CCD: 1, CPUs: []int{cpus[1]}}}}
}
