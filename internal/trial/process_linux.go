//go:build linux

package trial

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"syscall"
	"time"
)

type osHost struct {
	command func(context.Context, string, ...string) ([]byte, error)
}

func newOSHost() processHost { return osHost{command: commandOutput} }

func commandOutput(ctx context.Context, name string, args ...string) ([]byte, error) {
	out, err := exec.CommandContext(ctx, name, args...).CombinedOutput()
	if ctx.Err() != nil {
		err = ctx.Err()
	}
	return out, err
}

type execProcess struct {
	cmd            *exec.Cmd
	stdout, stderr *os.File
}

func (p execProcess) PID() int              { return p.cmd.Process.Pid }
func (p execProcess) Stdout() io.ReadCloser { return p.stdout }
func (p execProcess) Stderr() io.ReadCloser { return p.stderr }
func (p execProcess) Wait() error           { return p.cmd.Wait() }

func (osHost) Start(ctx context.Context, argv []string, dir string) (process, error) {
	cmd := exec.CommandContext(ctx, argv[0], argv[1:]...)
	cmd.Dir = dir
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true, Pdeathsig: syscall.SIGKILL}
	stdout, out, err := os.Pipe()
	if err != nil {
		return nil, fmt.Errorf("pipe stdout: %w", err)
	}
	defer out.Close()
	stderr, erout, err := os.Pipe()
	if err != nil {
		stdout.Close()
		return nil, fmt.Errorf("pipe stderr: %w", err)
	}
	defer erout.Close()
	cmd.Stdout, cmd.Stderr = out, erout
	if err := cmd.Start(); err != nil {
		stdout.Close()
		stderr.Close()
		return nil, err
	}
	return execProcess{cmd: cmd, stdout: stdout, stderr: stderr}, nil
}

func (osHost) SignalGroup(pid int, sig syscall.Signal) error { return syscall.Kill(-pid, sig) }

func (osHost) InScope(pid int, scope string) bool {
	b, err := os.ReadFile(fmt.Sprintf("/proc/%d/cgroup", pid))
	if err != nil {
		return false
	}
	for line := range strings.SplitSeq(strings.TrimSpace(string(b)), "\n") {
		if strings.HasSuffix(line, "/"+scope+".scope") {
			return true
		}
	}
	return false
}

func (osHost) Usage(pid int) (usage, error) {
	return procUsage(fmt.Sprintf("/proc/%d/stat", pid))
}

func procUsage(path string) (usage, error) {
	fields, err := procStat(path)
	if err != nil {
		return usage{}, fmt.Errorf("read usage %s: %w", path, err)
	}
	user, err := fieldInt(fields, 14)
	if err != nil {
		return usage{}, fmt.Errorf("read usage %s: %w", path, err)
	}
	system, err := fieldInt(fields, 15)
	if err != nil {
		return usage{}, fmt.Errorf("read usage %s: %w", path, err)
	}
	cpu, err := fieldInt(fields, 39)
	if err != nil {
		return usage{}, fmt.Errorf("read usage %s: %w", path, err)
	}
	return usage{CPUTime: time.Duration(user+system) * time.Second / 100, CPU: int(cpu)}, nil
}

func (osHost) Threads(pid int) ([]thread, error) {
	return procThreads(fmt.Sprintf("/proc/%d/task", pid))
}

func procThreads(path string) ([]thread, error) {
	tasks, err := os.ReadDir(path)
	if err != nil {
		return nil, fmt.Errorf("list threads %s: %w", path, err)
	}
	var threads []thread
	var lost error
	for _, task := range tasks {
		tid, err := strconv.Atoi(task.Name())
		if err != nil || tid <= 0 {
			lost = errors.Join(lost, fmt.Errorf("malformed thread id %s in %s", task.Name(), path))
			continue
		}
		stat, err := procStat(path + "/" + task.Name() + "/stat")
		if processDisappeared(err) {
			continue
		}
		if err != nil {
			lost = errors.Join(lost, fmt.Errorf("read thread %d: %w", tid, err))
			continue
		}
		cpu, err := fieldInt(stat, 39)
		if err != nil {
			lost = errors.Join(lost, fmt.Errorf("read thread %d: %w", tid, err))
			continue
		}
		threads = append(threads, thread{TID: tid, CPU: int(cpu)})
	}
	return threads, lost
}

func (h osHost) KillScope(ctx context.Context, scope string) ([]byte, error) {
	ctx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	args := []string{}
	if os.Geteuid() != 0 {
		args = append(args, "--user")
	}
	args = append(args, "kill", "--signal=SIGKILL", "--kill-whom=all", scope+".scope")
	return h.command(ctx, "systemctl", args...)
}

func procStat(path string) (fields []string, err error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	end := strings.LastIndexByte(string(b), ')')
	if end < 0 {
		return nil, fmt.Errorf("malformed proc stat %s", path)
	}
	fields = strings.Fields(string(b[end+1:]))
	if len(fields) < 37 {
		return nil, fmt.Errorf("short proc stat %s", path)
	}
	return fields, nil
}
func fieldInt(fields []string, number int) (int64, error) {
	v, err := strconv.ParseInt(fields[number-3], 10, 64)
	if err != nil {
		return 0, fmt.Errorf("malformed proc stat field %d: %w", number, err)
	}
	if v < 0 {
		return 0, fmt.Errorf("negative proc stat field %d: %d", number, v)
	}
	return v, nil
}
func CheckSystemdRun() (string, error) {
	return checkSystemdRun(commandOutput)
}

func checkSystemdRun(command func(context.Context, string, ...string) ([]byte, error)) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	argv := scopeArgv(fmt.Sprintf("togi-preflight-%d", os.Getpid()), []int{0}, "/bin/sh", "-c", "exit 0")
	out, err := command(ctx, argv[0], argv[1:]...)
	if err != nil {
		return "", fmt.Errorf("systemd-run: %w: %s", err, strings.TrimSpace(string(out)))
	}
	return "scope confined to cpu 0 created", nil
}
