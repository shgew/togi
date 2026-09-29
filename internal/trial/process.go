package trial

import (
	"context"
	"io"
	"syscall"
	"time"
)

type process interface {
	PID() int
	Stdout() io.ReadCloser
	Stderr() io.ReadCloser
	Wait() error
}

type processHost interface {
	Start(ctx context.Context, argv []string, dir string) (process, error)
	SignalGroup(pid int, sig syscall.Signal) error
	InScope(pid int, scope string) bool
	Usage(pid int) (usage, error)
	Threads(pid int) ([]thread, error)
	KillScope(ctx context.Context, scope string) ([]byte, error)
	ListScopes(ctx context.Context) ([]string, error)
	ScopeProcesses(ctx context.Context) ([]scopeProcess, error)
	ProcessAlive(scopeProcess) (bool, error)
	StopScope(ctx context.Context, scope string) ([]byte, error)
}

type usage struct {
	CPUTime time.Duration
	CPU     int
}

type thread struct{ TID, CPU int }

type scopeProcess struct {
	Scope      string
	PID, Group int
	Start      int64
}
