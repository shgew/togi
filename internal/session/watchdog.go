package session

import (
	"context"
	"fmt"
	"time"

	"github.com/shgew/togi/internal/journal"
)

func (r *runner) waitWatchdog(ctx context.Context) (*Stop, error) {
	start := r.in.Machine.Clock.Monotonic()
	for {
		if ctx.Err() != nil {
			stop, err := r.shutdown(&journal.Shutdown{Reason: journal.ShutdownSignal}, StopSignal)
			return &stop, err
		}
		check := r.in.Machine.Host.Watchdog()
		waited := r.in.Machine.Clock.Monotonic() - start
		if check.OK || waited >= 30*time.Second {
			detail := fmt.Sprintf("%s; waited %s for an armed hardware watchdog", check.Detail, waited)
			e, err := r.append(&journal.PreflightCheck{Check: "watchdog", Detail: detail, OK: check.OK})
			if err != nil {
				return nil, err
			}
			if !check.OK {
				return r.deadEnd(&journal.DeadEnd{Condition: journal.DeadEndPreflight, Detail: "failed checks: watchdog (" + detail + ")"}, e.Seq)
			}
			return nil, nil
		}
		if err := r.in.Machine.Clock.Sleep(ctx, min(time.Second, 30*time.Second-waited)); err != nil {
			if ctx.Err() != nil {
				stop, shutdownErr := r.shutdown(&journal.Shutdown{Reason: journal.ShutdownSignal}, StopSignal)
				return &stop, shutdownErr
			}
			return nil, fmt.Errorf("wait for hardware watchdog: %w", err)
		}
	}
}
