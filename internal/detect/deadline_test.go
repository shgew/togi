package detect

import (
	"context"
	"errors"
	"testing"
	"testing/synctest"
	"time"
)

func TestKernelReadTimeout(t *testing.T) {
	for _, operation := range []string{"mces", "reset"} {
		t.Run(operation, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				started := time.Now()
				k := NewKernel(nil)
				k.journalctl = func(args []string) ([]byte, []byte, int, error) {
					return journalctlWithDeadline(args, func(ctx context.Context, _ []string) ([]byte, []byte, int, error) {
						deadline, ok := ctx.Deadline()
						if !ok || deadline.Sub(started) != 30*time.Second {
							t.Fatalf("deadline = %v, present %v", deadline, ok)
						}
						<-ctx.Done()
						return nil, nil, 1, ctx.Err()
					})
				}
				var err error
				if operation == "mces" {
					_, err = k.MCEs("boot", 0)
				} else {
					_, err = k.ResetReason("boot")
				}
				if !errors.Is(err, context.DeadlineExceeded) {
					t.Fatalf("kernel read = %v", err)
				}
			})
		})
	}
}
