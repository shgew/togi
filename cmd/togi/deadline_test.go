package main

import (
	"context"
	"errors"
	"testing"
	"testing/synctest"
	"time"
)

func TestRebootTimeout(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		started := time.Now()
		_, err := rebootSystem(func(ctx context.Context, _ string, _ ...string) ([]byte, error) {
			deadline, ok := ctx.Deadline()
			if !ok || deadline.Sub(started) != 30*time.Second {
				t.Fatalf("deadline = %v, present %v", deadline, ok)
			}
			<-ctx.Done()
			return nil, ctx.Err()
		})
		if !errors.Is(err, context.DeadlineExceeded) {
			t.Fatalf("reboot = %v", err)
		}
	})
}
