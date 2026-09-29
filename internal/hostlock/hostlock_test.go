package hostlock

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"sync"
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
