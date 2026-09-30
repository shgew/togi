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
	"sync"
	"syscall"
	"time"

	"golang.org/x/sys/unix"
)

type osHost struct {
	command func(context.Context, string, ...string) ([]byte, error)
	procDir string
}

func newOSHost() processHost { return osHost{command: commandOutput} }

func commandOutput(ctx context.Context, name string, args ...string) ([]byte, error) {
	cmd := exec.CommandContext(ctx, name, args...)
	if os.Geteuid() == 0 {
		cmd.SysProcAttr = &syscall.SysProcAttr{Credential: &syscall.Credential{Uid: 0, Gid: uint32(os.Getegid())}}
	}
	out, err := cmd.CombinedOutput()
	if ctx.Err() != nil {
		err = ctx.Err()
	}
	return out, err
}

type execProcess struct {
	cmd            *exec.Cmd
	stdout, stderr *os.File
	start          int64
	mu             sync.Mutex
	reaped         bool
}

func (p *execProcess) PID() int              { return p.cmd.Process.Pid }
func (p *execProcess) Stdout() io.ReadCloser { return p.stdout }
func (p *execProcess) Stderr() io.ReadCloser { return p.stderr }
func (p *execProcess) Wait() error {
	var info unix.Siginfo
	var waitErr error
	for {
		waitErr = unix.Waitid(unix.P_PID, p.PID(), &info, unix.WEXITED|unix.WNOWAIT, nil)
		if !errors.Is(waitErr, syscall.EINTR) {
			break
		}
	}
	p.mu.Lock()
	p.reaped = true
	p.mu.Unlock()
	if waitErr != nil {
		waitErr = errors.Join(fmt.Errorf("wait for owned process %d: %w", p.PID(), waitErr), p.cmd.Process.Kill())
	}
	return errors.Join(waitErr, p.cmd.Wait())
}

func (osHost) Start(ctx context.Context, argv []string, dir string) (process, error) {
	cmd := exec.CommandContext(ctx, argv[0], argv[1:]...)
	cmd.Dir = dir
	cmd.SysProcAttr = launcherAttributes(os.Geteuid(), os.Getegid())
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
	fields, err := procStat(fmt.Sprintf("/proc/%d/stat", cmd.Process.Pid))
	var start int64
	if err == nil {
		start, err = strconv.ParseInt(fields[19], 10, 64)
	}
	if err == nil && start < 0 {
		err = fmt.Errorf("invalid start time %d", start)
	}
	if err != nil || start < 0 {
		killErr := cmd.Process.Kill()
		waitErr := cmd.Wait()
		stdout.Close()
		stderr.Close()
		return nil, errors.Join(fmt.Errorf("capture owned process identity: %w", err), killErr, waitErr)
	}
	return &execProcess{cmd: cmd, stdout: stdout, stderr: stderr, start: start}, nil
}

func launcherAttributes(uid, gid int) *syscall.SysProcAttr {
	attr := &syscall.SysProcAttr{Setpgid: true, Pdeathsig: syscall.SIGKILL}
	if uid == 0 {
		attr.Credential = &syscall.Credential{Uid: 0, Gid: uint32(gid)}
	}
	return attr
}

func (h osHost) SignalGroup(owned process, sig syscall.Signal) error {
	p, ok := owned.(*execProcess)
	if !ok {
		return errors.New("signal process group: process was not launched by osHost")
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.reaped {
		return syscall.ESRCH
	}
	fields, err := procStat(h.procPath(p.PID(), "stat"))
	if os.IsNotExist(err) {
		return syscall.ESRCH
	}
	if err != nil {
		return fmt.Errorf("verify owned process %d identity: %w", p.PID(), err)
	}
	start, err := strconv.ParseInt(fields[19], 10, 64)
	if err != nil {
		return fmt.Errorf("verify owned process %d start time: %w", p.PID(), err)
	}
	if start != p.start || fields[0] == "Z" || fields[0] == "X" {
		return syscall.ESRCH
	}
	group, err := strconv.Atoi(fields[2])
	if err != nil || group != p.PID() || group <= 0 {
		return fmt.Errorf("owned process %d no longer leads its process group", p.PID())
	}
	return syscall.Kill(-group, sig)
}

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
func CheckSystemdRun(user Identity) (string, error) {
	return checkSystemdRun(user, commandOutput)
}

func checkSystemdRun(user Identity, command func(context.Context, string, ...string) ([]byte, error)) (string, error) {
	if err := user.validate(); err != nil {
		return "", fmt.Errorf("systemd-run backend_user: %w", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	argv := scopeArgv(fmt.Sprintf("togi-preflight-%d", os.Getpid()), []int{0}, user, "/", "/bin/sh", "-c", "exit 0")
	out, err := command(ctx, argv[0], argv[1:]...)
	if err != nil {
		return "", fmt.Errorf("systemd-run: %w: %s", err, strings.TrimSpace(string(out)))
	}
	return "scope confined to cpu 0 created", nil
}
