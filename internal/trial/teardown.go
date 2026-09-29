package trial

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"syscall"
	"time"
)

const teardownLimit = 15 * time.Second

func terminate(host processHost, instances []*instance, scopes []string, deadline time.Time, grace time.Duration, collect func(time.Time) bool) error {
	for _, inst := range instances {
		_ = host.SignalGroup(inst.PID, syscall.SIGCONT)
		_ = host.SignalGroup(inst.PID, syscall.SIGTERM)
	}
	collect(minTime(deadline, time.Now().Add(grace)))
	ctx, cancel := context.WithDeadline(context.Background(), minTime(deadline, time.Now().Add(2*time.Second)))
	defer cancel()
	var cleanupErr error
	for _, scope := range scopes {
		out, err := host.KillScope(ctx, scope)
		if err != nil && !scopeMissing(out, err) {
			cleanupErr = errors.Join(cleanupErr, fmt.Errorf("kill scope %s: %w: %s", scope, err, strings.TrimSpace(string(out))))
		}
	}
	for _, inst := range instances {
		if err := host.SignalGroup(inst.PID, syscall.SIGKILL); err != nil && !errors.Is(err, syscall.ESRCH) {
			cleanupErr = errors.Join(cleanupErr, fmt.Errorf("kill process group %d: %w", inst.PID, err))
		}
	}
	if !collect(minTime(deadline, time.Now().Add(10*time.Second))) {
		cleanupErr = errors.Join(cleanupErr, errors.New("backend exit and output drain not confirmed before teardown deadline"))
	}
	return cleanupErr
}

func scopeMissing(out []byte, err error) bool {
	return !errors.Is(err, context.DeadlineExceeded) && !errors.Is(err, context.Canceled) &&
		(strings.Contains(string(out), "not loaded") || strings.Contains(string(out), "could not be found"))
}

func minTime(a, b time.Time) time.Time {
	if a.Before(b) {
		return a
	}
	return b
}
