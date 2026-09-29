package hostlock

import (
	"errors"
	"fmt"
	"os"
	"syscall"
)

const Path = "/run/lock/togi.lock"

var ErrLocked = errors.New("another togi process holds the host lock")

func Acquire(path string) (*os.File, error) {
	lock, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o644)
	if err != nil {
		return nil, fmt.Errorf("open host lock %s: %w", path, err)
	}
	if err := syscall.Flock(int(lock.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		_ = lock.Close()
		if errors.Is(err, syscall.EWOULDBLOCK) {
			err = ErrLocked
		}
		return nil, fmt.Errorf("acquire host lock %s: %w", path, err)
	}
	return lock, nil
}
