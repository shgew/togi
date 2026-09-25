//go:build (integration || hardware) && linux

package trial

import (
	"fmt"
	"os"
	"os/exec"
	"runtime"
	"slices"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"
	"unsafe"

	"code.marleb.org/shgew/shycler/internal/backend"
	"code.marleb.org/shgew/shycler/internal/machine"
)

type helperBackend struct{ mode string }

func (h helperBackend) Name() string           { return "helper" }
func (h helperBackend) Check() (string, error) { return "ok", nil }
func (h helperBackend) Prepare(_ machine.Workload, _ string, cpus []int) (backend.Launch, error) {
	launch := backend.Launch{Argv: []string{"taskset", "-c", strconv.Itoa(cpus[0]), os.Args[0], "-test.run=TestHelperProcess", "--", "--helper", h.mode}}
	if strings.HasPrefix(h.mode, "watched") {
		launch.Watch = []string{"results.txt"}
	}
	return launch, nil
}
func (h helperBackend) Classify(line string) backend.Line { return classifyHelper(line) }

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
	case "descendant":
		cmd := exec.Command(os.Args[0], "-test.run=TestHelperProcess", "--", "--helper", "orphan")
		cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true}
		if err := cmd.Start(); err != nil {
			os.Exit(2)
		}
		if err := os.WriteFile("descendant.pid", []byte(strconv.Itoa(cmd.Process.Pid)), 0644); err != nil {
			os.Exit(2)
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
	return Options{Dir: t.TempDir(), NoScope: true, Backends: map[machine.Backend]backend.Backend{machine.Mprime: helperBackend{mode}}, SampleInterval: 50 * time.Millisecond, StallGrace: time.Hour, Cores: []machine.CoreInfo{{Core: 0, CCD: 0, CPUs: []int{cpus[0]}}, {Core: 1, CCD: 1, CPUs: []int{cpus[1]}}}}
}
