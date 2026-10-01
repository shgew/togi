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

var errOutputDrainUnconfirmed = errors.New("backend exit and output drain not confirmed before teardown deadline")

// Collection runs during grace and after kill attempts. Watched partial lines
// become final evidence only for instances whose owned writers were stopped.
func terminate(host processHost, instances []*instance, scopes []string, deadline time.Time, grace time.Duration, recovered bool, collect func(until time.Time, final bool) bool) error {
	graceDeadline := minTime(deadline, time.Now().Add(grace))
	var cleanupErr error
	if recovered {
		cleanupErr = signalRecoveredScopes(host, scopes, graceDeadline)
	} else {
		for _, inst := range instances {
			_ = host.SignalGroup(inst.process, syscall.SIGCONT)
			_ = host.SignalGroup(inst.process, syscall.SIGTERM)
		}
	}
	collect(graceDeadline, false)
	ctx, cancel := context.WithDeadline(context.Background(), minTime(deadline, time.Now().Add(2*time.Second)))
	defer cancel()
	for _, scope := range scopes {
		out, err := host.KillScope(ctx, scope)
		if err != nil && !scopeMissing(out, err) {
			cleanupErr = errors.Join(cleanupErr, fmt.Errorf("kill scope %s: %w: %s", scope, err, strings.TrimSpace(string(out))))
			continue
		}
		for _, inst := range instances {
			if inst.Scope == scope {
				inst.writersStopped = true
			}
		}
	}
	for _, inst := range instances {
		err := host.SignalGroup(inst.process, syscall.SIGKILL)
		if len(scopes) == 0 {
			// NoScope helpers require a verified group kill. ESRCH may mean
			// only the launcher exited, not that descendant writers stopped.
			inst.writersStopped = err == nil
		}
		if err != nil && !errors.Is(err, syscall.ESRCH) {
			inst.writersStopped = false
			cleanupErr = errors.Join(cleanupErr, fmt.Errorf("kill process group %d: %w", inst.PID, err))
		}
	}
	if !collect(minTime(deadline, time.Now().Add(10*time.Second)), true) {
		cleanupErr = errors.Join(cleanupErr, errOutputDrainUnconfirmed)
	}
	return cleanupErr
}

func signalRecoveredScopes(host processHost, scopes []string, deadline time.Time) error {
	ctx, cancel := context.WithDeadline(context.Background(), deadline)
	defer cancel()
	var cleanupErr error
	for _, sig := range []syscall.Signal{syscall.SIGCONT, syscall.SIGTERM} {
		for _, scope := range scopes {
			out, err := host.SignalScope(ctx, scope, sig)
			if err != nil && !scopeMissing(out, err) {
				cleanupErr = errors.Join(cleanupErr, fmt.Errorf("signal scope %s: %w: %s", scope, err, strings.TrimSpace(string(out))))
			}
		}
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
