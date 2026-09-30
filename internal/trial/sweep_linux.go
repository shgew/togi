//go:build linux

package trial

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
)

func (h osHost) ListScopes(ctx context.Context) ([]string, error) {
	args := []string{}
	if os.Geteuid() != 0 {
		args = append(args, "--user")
	}
	args = append(args, "list-units", "--all", "--type=scope", "--no-legend", "--plain", "--no-pager", "togi-trial-*.scope")
	out, err := h.command(ctx, "systemctl", args...)
	if err != nil {
		return nil, fmt.Errorf("systemctl list-units: %w: %s", err, strings.TrimSpace(string(out)))
	}
	var units []string
	for line := range strings.SplitSeq(string(out), "\n") {
		fields := strings.Fields(line)
		if len(fields) > 0 && trialScope(fields[0]) {
			units = append(units, fields[0])
		}
	}
	return units, nil
}

func (h osHost) StopScope(ctx context.Context, scope string) ([]byte, error) {
	args := []string{}
	if os.Geteuid() != 0 {
		args = append(args, "--user")
	}
	args = append(args, "stop", scope+".scope")
	return h.command(ctx, "systemctl", args...)
}

func (h osHost) SignalScope(ctx context.Context, scope string, sig syscall.Signal) ([]byte, error) {
	args := []string{}
	if os.Geteuid() != 0 {
		args = append(args, "--user")
	}
	args = append(args, "kill", "--signal="+strconv.Itoa(int(sig)), "--kill-whom=all", scope+".scope")
	return h.command(ctx, "systemctl", args...)
}

func (h osHost) procPath(pid int, name string) string {
	root := h.procDir
	if root == "" {
		root = "/proc"
	}
	if pid == 0 {
		return root
	}
	return filepath.Join(root, strconv.Itoa(pid), name)
}

func (h osHost) ScopeProcesses(ctx context.Context) ([]scopeProcess, error) {
	entries, err := os.ReadDir(h.procPath(0, ""))
	if err != nil {
		return nil, fmt.Errorf("list processes: %w", err)
	}
	var processes []scopeProcess
	for _, entry := range entries {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		pid, err := strconv.Atoi(entry.Name())
		if err != nil || pid <= 0 {
			continue
		}
		data, err := os.ReadFile(h.procPath(pid, "cgroup"))
		if os.IsNotExist(err) {
			continue
		}
		if err != nil {
			return nil, fmt.Errorf("read process %d cgroup: %w", pid, err)
		}
		unit := ""
		for line := range strings.SplitSeq(string(data), "\n") {
			fields := strings.SplitN(line, ":", 3)
			if len(fields) != 3 {
				continue
			}
			for component := range strings.SplitSeq(fields[2], "/") {
				if trialScope(component) {
					unit = component
				}
			}
		}
		if unit == "" {
			continue
		}
		fields, err := procStat(h.procPath(pid, "stat"))
		if os.IsNotExist(err) {
			continue
		}
		if err != nil {
			return nil, fmt.Errorf("read leftover process %d identity: %w", pid, err)
		}
		if fields[0] == "Z" || fields[0] == "X" {
			continue
		}
		start, err := strconv.ParseInt(fields[19], 10, 64)
		if err != nil || start < 0 {
			return nil, fmt.Errorf("invalid leftover process %d start time %q", pid, fields[19])
		}
		processes = append(processes, scopeProcess{Scope: unit, PID: pid, Start: start})
	}
	return processes, ctx.Err()
}

func (h osHost) ProcessAlive(p scopeProcess) (bool, error) {
	fields, err := procStat(h.procPath(p.PID, "stat"))
	if os.IsNotExist(err) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	start, err := strconv.ParseInt(fields[19], 10, 64)
	if err != nil {
		return false, fmt.Errorf("read process %d start time: %w", p.PID, err)
	}
	return fields[0] != "Z" && fields[0] != "X" && start == p.Start, nil
}
