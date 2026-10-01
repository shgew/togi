package hostlock

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"syscall"

	"golang.org/x/sys/unix"
)

var ErrLocked = errors.New("another togi process holds the host lock")

func Acquire(path string) (*os.File, error) {
	parent, err := openParent(filepath.Dir(path))
	if err != nil {
		return nil, fmt.Errorf("open host lock parent %s: %w", path, err)
	}
	defer parent.Close()
	requireRoot := runtime.GOOS == "linux" && path == Path
	flags := unix.O_NOFOLLOW | unix.O_NONBLOCK | unix.O_CLOEXEC
	name := filepath.Base(path)
	fd, err := unix.Openat(int(parent.Fd()), name, flags|unix.O_RDWR, 0)
	writable := true
	if errors.Is(err, os.ErrNotExist) {
		if requireRoot && os.Geteuid() != 0 {
			return nil, fmt.Errorf("create host lock %s: root must provision the lock: %w", path, os.ErrPermission)
		}
		fd, err = unix.Openat(int(parent.Fd()), name, flags|unix.O_RDWR|unix.O_CREAT|unix.O_EXCL, 0o600)
		if errors.Is(err, os.ErrExist) {
			fd, err = unix.Openat(int(parent.Fd()), name, flags|unix.O_RDWR, 0)
		}
	}
	if errors.Is(err, os.ErrPermission) {
		fd, err = unix.Openat(int(parent.Fd()), name, flags|unix.O_RDONLY, 0)
		writable = false
	}
	if err != nil {
		return nil, fmt.Errorf("open host lock %s: %w", path, err)
	}
	lock := os.NewFile(uintptr(fd), path)
	if err := secureLock(lock, requireRoot, writable); err != nil {
		_ = lock.Close()
		return nil, fmt.Errorf("secure host lock %s: %w", path, err)
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

func openParent(path string) (*os.File, error) {
	resolved, err := filepath.EvalSymlinks(path)
	if err != nil {
		return nil, err
	}
	resolved, err = filepath.Abs(resolved)
	if err != nil {
		return nil, err
	}
	parent, err := os.OpenFile("/", os.O_RDONLY|syscall.O_DIRECTORY, 0)
	if err != nil {
		return nil, err
	}
	remaining := strings.TrimPrefix(resolved, "/")
	caller := uint32(os.Geteuid())
	for {
		info, err := parent.Stat()
		if err == nil {
			stat := info.Sys().(*syscall.Stat_t)
			err = validateDirectory(info.Mode(), stat.Uid, caller, remaining == "", parent.Name() == "/")
			if err != nil {
				err = fmt.Errorf("directory %s (uid %d, mode %s): %w", parent.Name(), stat.Uid, info.Mode(), err)
			}
		}
		if err != nil {
			_ = parent.Close()
			return nil, err
		}
		component, rest, _ := strings.Cut(remaining, "/")
		if component == "" {
			return parent, nil
		}
		fd, err := unix.Openat(int(parent.Fd()), component, unix.O_RDONLY|unix.O_DIRECTORY|unix.O_NOFOLLOW|unix.O_CLOEXEC, 0)
		_ = parent.Close()
		if err != nil {
			return nil, err
		}
		componentPath := resolved[:len(resolved)-len(remaining)+len(component)]
		parent = os.NewFile(uintptr(fd), componentPath)
		remaining = rest
	}
}

func validateDirectory(mode os.FileMode, uid, caller uint32, containing, namespaceRoot bool) error {
	if mode.Perm()&0o022 != 0 && mode&os.ModeSticky == 0 {
		return errors.New("writable directory lacks the sticky bit")
	}
	// The namespace root's authority does not depend on how its owner is
	// mapped into the caller's namespace. The lock's containing directory
	// still requires an authorized owner, even when it is the namespace root.
	if uid != 0 && uid != caller && (containing || (!namespaceRoot && mode.Perm()&0o222 != 0)) {
		return errors.New("directory is owned by another user")
	}
	return nil
}

func secureLock(lock *os.File, requireRoot, writable bool) error {
	info, err := lock.Stat()
	if err != nil {
		return err
	}
	stat := info.Sys().(*syscall.Stat_t)
	if !info.Mode().IsRegular() || stat.Nlink != 1 {
		return errors.New("lock must be a regular file with one link")
	}
	if (requireRoot && stat.Uid != 0) || (stat.Uid != 0 && stat.Uid != uint32(os.Geteuid())) {
		return errors.New("lock is owned by another user")
	}
	mode := info.Mode()
	if mode != 0o600 && mode != 0o660 {
		if stat.Uid != uint32(os.Geteuid()) {
			return fmt.Errorf("lock owner must restrict legacy permissions: %w", os.ErrPermission)
		}
		if err := lock.Chmod(0o600); err != nil {
			return err
		}
	} else if !writable {
		return fmt.Errorf("lock requires read-write access: %w", os.ErrPermission)
	}
	return nil
}
