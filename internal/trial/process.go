package trial

import (
	"context"
	"io"
	"os"
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
	Chown(path string, uid, gid int) error
	SignalGroup(process, syscall.Signal) error
	InScope(pid int, scope string) bool
	Usage(pid int) (usage, error)
	Threads(pid int) ([]thread, error)
	KillScope(ctx context.Context, scope string) ([]byte, error)
	SignalScope(ctx context.Context, scope string, sig syscall.Signal) ([]byte, error)
	ListScopes(ctx context.Context) ([]string, error)
	ScopeProcesses(ctx context.Context) ([]scopeProcess, error)
	ProcessAlive(scopeProcess) (bool, error)
	StopScope(ctx context.Context, scope string) ([]byte, error)
}

func (osHost) Chown(path string, uid, gid int) error { return os.Chown(path, uid, gid) }

type usage struct {
	CPUTime time.Duration
	CPU     int
}

type thread struct{ TID, CPU int }

type scopeProcess struct {
	Scope string
	PID   int
	Start int64
}
