package hostlock

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestExclusiveUntilClosed(t *testing.T) {
	t.Parallel()
	path := filepath.Join(t.TempDir(), "togi.lock")
	lock, err := Acquire(path)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = lock.Close() })
	before, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if before.Mode().Perm() != 0o600 {
		t.Fatalf("new lock permissions %o, want 600", before.Mode().Perm())
	}
	const contenders = 8
	var wg sync.WaitGroup
	for range contenders {
		wg.Go(func() {
			other, err := Acquire(path)
			if other != nil {
				_ = other.Close()
				t.Error("contender acquired an owned host lock")
			}
			if !errors.Is(err, ErrLocked) {
				t.Errorf("contender error %v, want ErrLocked", err)
			} else if !strings.Contains(err.Error(), path) {
				t.Errorf("contention did not name lock %s: %v", path, err)
			}
		})
	}
	wg.Wait()
	if err := lock.Close(); err != nil {
		t.Fatal(err)
	}
	other, err := Acquire(path)
	if err != nil {
		t.Fatalf("acquire released lock: %v", err)
	}
	if err := other.Close(); err != nil {
		t.Fatal(err)
	}
	after, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if !os.SameFile(before, after) {
		t.Fatal("release replaced or removed the lock inode")
	}
}

func TestLegacyLockMigrationPreservesInodeAndContents(t *testing.T) {
	t.Parallel()
	for _, mode := range []os.FileMode{0o644, 0o444, 0o640} {
		t.Run(mode.String(), func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "togi.lock")
			const contents = "persistent host lock\n"
			if err := os.WriteFile(path, []byte(contents), mode); err != nil {
				t.Fatal(err)
			}
			if err := os.Chmod(path, mode); err != nil {
				t.Fatal(err)
			}
			before, err := os.Stat(path)
			if err != nil {
				t.Fatal(err)
			}
			lock, err := Acquire(path)
			if err != nil {
				t.Fatalf("acquire legacy lock: %v", err)
			}
			if err := lock.Close(); err != nil {
				t.Fatal(err)
			}
			after, err := os.Stat(path)
			if err != nil {
				t.Fatal(err)
			}
			if !os.SameFile(before, after) || after.Mode().Perm() != 0o600 {
				t.Fatal("migration replaced the inode or left legacy permissions")
			}
			got, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(contents, string(got)); diff != "" {
				t.Errorf("lock contents changed (-want +got): %s", diff)
			}
		})
	}
}

func TestLegacyReadOnlyHolderRemainsExclusiveDuringMigration(t *testing.T) {
	t.Parallel()
	path := filepath.Join(t.TempDir(), "togi.lock")
	if err := os.WriteFile(path, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(path, 0o644); err != nil {
		t.Fatal(err)
	}
	holder, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = holder.Close() })
	before, err := holder.Stat()
	if err != nil {
		t.Fatal(err)
	}
	if err := syscall.Flock(int(holder.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		t.Fatal(err)
	}
	for range 2 {
		lock, err := Acquire(path)
		if lock != nil {
			_ = lock.Close()
			t.Fatal("migration bypassed a read-only legacy holder")
		}
		if !errors.Is(err, ErrLocked) {
			t.Fatalf("migration error %v, want ErrLocked", err)
		}
	}
	after, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if !os.SameFile(before, after) || after.Mode().Perm() != 0o600 {
		t.Fatal("held legacy inode was replaced or not restricted")
	}
	if err := holder.Close(); err != nil {
		t.Fatal(err)
	}
	lock, err := Acquire(path)
	if err != nil {
		t.Fatalf("acquire after legacy holder exits: %v", err)
	}
	_ = lock.Close()
}

func TestProvisionedGroupLockPreservesPermissions(t *testing.T) {
	t.Parallel()
	path := filepath.Join(t.TempDir(), "togi.lock")
	if err := os.WriteFile(path, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(path, 0o660); err != nil {
		t.Fatal(err)
	}
	lock, err := Acquire(path)
	if err != nil {
		t.Fatal(err)
	}
	_ = lock.Close()
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode().Perm() != 0o660 {
		t.Fatalf("authorized group permissions changed to %o", info.Mode().Perm())
	}
}

func TestConcurrentCreationHasOneOwner(t *testing.T) {
	t.Parallel()
	path := filepath.Join(t.TempDir(), "togi.lock")
	const contenders = 8
	results := make(chan *os.File, contenders)
	start := make(chan struct{})
	var wg sync.WaitGroup
	for range contenders {
		wg.Go(func() {
			<-start
			lock, err := Acquire(path)
			if err != nil && !errors.Is(err, ErrLocked) {
				t.Errorf("concurrent acquisition: %v", err)
			}
			results <- lock
		})
	}
	close(start)
	wg.Wait()
	close(results)
	owners := 0
	for lock := range results {
		if lock != nil {
			owners++
			_ = lock.Close()
		}
	}
	if owners != 1 {
		t.Fatalf("%d concurrent owners, want one", owners)
	}
}

func TestUnsafeLockObjectsAreNotModified(t *testing.T) {
	t.Parallel()
	for _, kind := range []string{"symlink", "hardlink", "directory", "fifo"} {
		t.Run(kind, func(t *testing.T) {
			dir := t.TempDir()
			target := filepath.Join(dir, "target")
			if err := os.WriteFile(target, []byte("unchanged"), 0o644); err != nil {
				t.Fatal(err)
			}
			if err := os.Chmod(target, 0o644); err != nil {
				t.Fatal(err)
			}
			path := filepath.Join(dir, "togi.lock")
			var err error
			switch kind {
			case "symlink":
				err = os.Symlink(target, path)
			case "hardlink":
				err = os.Link(target, path)
			case "directory":
				err = os.Mkdir(path, 0o700)
			case "fifo":
				err = syscall.Mkfifo(path, 0o600)
			}
			if err != nil {
				t.Fatal(err)
			}
			before, err := os.Lstat(path)
			if err != nil {
				t.Fatal(err)
			}
			lock, err := Acquire(path)
			if lock != nil {
				_ = lock.Close()
				t.Fatal("acquired an unsafe lock object")
			}
			if err == nil || errors.Is(err, ErrLocked) {
				t.Fatalf("unsafe object error: %v", err)
			}
			after, err := os.Lstat(path)
			if err != nil {
				t.Fatal(err)
			}
			if !os.SameFile(before, after) || before.Mode() != after.Mode() {
				t.Fatal("unsafe object was replaced or chmodded")
			}
			info, err := os.Stat(target)
			if err != nil {
				t.Fatal(err)
			}
			data, err := os.ReadFile(target)
			if err != nil {
				t.Fatal(err)
			}
			if info.Mode().Perm() != 0o644 || string(data) != "unchanged" {
				t.Fatal("unsafe object's target was modified")
			}
		})
	}
}

func TestWritableParentNeedsStickyBit(t *testing.T) {
	t.Parallel()
	for _, sticky := range []bool{false, true} {
		t.Run(fmt.Sprint(sticky), func(t *testing.T) {
			dir := t.TempDir()
			mode := os.FileMode(0o777)
			if sticky {
				mode |= os.ModeSticky
			}
			if err := os.Chmod(dir, mode); err != nil {
				t.Fatal(err)
			}
			child := filepath.Join(dir, "child")
			if err := os.Mkdir(child, 0o700); err != nil {
				t.Fatal(err)
			}
			path := filepath.Join(child, "togi.lock")
			lock, err := Acquire(path)
			if sticky {
				if err != nil {
					t.Fatal(err)
				}
				_ = lock.Close()
			} else {
				if lock != nil {
					_ = lock.Close()
					t.Fatal("acquired lock below a writable nonsticky ancestor")
				}
				if err == nil {
					t.Fatal("unsafe parent was accepted")
				}
				if _, err := os.Stat(path); !errors.Is(err, os.ErrNotExist) {
					t.Fatalf("unsafe parent created a lock: %v", err)
				}
			}
		})
	}
}

func TestLockOpenFailureCreatesNoDirectory(t *testing.T) {
	t.Parallel()
	dir := filepath.Join(t.TempDir(), "missing")
	path := filepath.Join(dir, "togi.lock")
	lock, err := Acquire(path)
	if lock != nil {
		_ = lock.Close()
		t.Fatal("acquired lock in a missing directory")
	}
	if !errors.Is(err, os.ErrNotExist) || !strings.Contains(err.Error(), path) {
		t.Fatalf("open failure %v, want missing named lock", err)
	}
	_, err = os.Stat(dir)
	if diff := cmp.Diff(true, errors.Is(err, os.ErrNotExist)); diff != "" {
		t.Errorf("parent directory created (-want +got): %s", diff)
	}
}
