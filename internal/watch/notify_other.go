//go:build !linux

package watch

import (
	"context"
	"time"
)

// watchJournal calls load for the first frame, then signals once a second where filesystem notifications are
// unavailable; the caller's reload decides whether the journal changed.
func watchJournal(ctx context.Context, _ string, load func()) (<-chan error, func(), error) {
	load()
	ctx, cancel := context.WithCancel(ctx)
	changes, done := make(chan error), make(chan struct{})
	go func() {
		defer close(done)
		ticker := time.NewTicker(time.Second)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
			}
			select {
			case changes <- nil:
			case <-ctx.Done():
				return
			}
		}
	}()
	return changes, func() { cancel(); <-done }, nil
}
