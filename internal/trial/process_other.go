//go:build !linux

package trial

import (
	"context"
	"errors"
	"fmt"
	"syscall"
)

type osHost struct{}

func newOSHost() processHost { return osHost{} }

func (osHost) Start(context.Context, []string, string) (process, error) {
	return nil, fmt.Errorf("start trial process: %w", errors.ErrUnsupported)
}
func (osHost) SignalGroup(int, syscall.Signal) error {
	return fmt.Errorf("signal trial process: %w", errors.ErrUnsupported)
}
func (osHost) InScope(int, string) bool { return false }
func (osHost) Usage(int) (usage, error) {
	return usage{}, fmt.Errorf("read trial cpu usage: %w", errors.ErrUnsupported)
}
func (osHost) Threads(int) ([]thread, error) {
	return nil, fmt.Errorf("list trial threads: %w", errors.ErrUnsupported)
}
func (osHost) KillScope(context.Context, string) ([]byte, error) {
	return nil, fmt.Errorf("kill trial scope: %w", errors.ErrUnsupported)
}
func (osHost) ListScopes(context.Context) ([]string, error) {
	return nil, fmt.Errorf("list trial scopes: %w", errors.ErrUnsupported)
}
func (osHost) ScopeProcesses(context.Context) ([]scopeProcess, error) {
	return nil, fmt.Errorf("list trial scope processes: %w", errors.ErrUnsupported)
}
func (osHost) ProcessAlive(scopeProcess) (bool, error) {
	return false, fmt.Errorf("verify trial process exit: %w", errors.ErrUnsupported)
}
func (osHost) StopScope(context.Context, string) ([]byte, error) {
	return nil, fmt.Errorf("stop trial scope: %w", errors.ErrUnsupported)
}
