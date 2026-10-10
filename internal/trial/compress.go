package trial

import (
	"compress/gzip"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"syscall"

	"github.com/shgew/togi/internal/machine"
)

// compressTrialFile replaces the regular file at path with its gzip form at path+".gz". The compressed file is written
// whole under a temporary name, synced and renamed into place before the plain file is removed, so a reader finds one
// complete form at every moment and a crash leaves either both (readers prefer the plain one) or the compressed one.
// A missing path is skipped. So is anything that is not a regular file: a workload may replace its logs with a
// symlink, which is never followed.
func compressTrialFile(path string) error {
	src, err := os.OpenFile(path, os.O_RDONLY|syscall.O_NOFOLLOW|syscall.O_NONBLOCK, 0)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) || errors.Is(err, syscall.ELOOP) {
			return nil
		}
		return fmt.Errorf("open %s: %w", filepath.Base(path), err)
	}
	defer src.Close()
	if info, err := src.Stat(); err != nil || !info.Mode().IsRegular() {
		if err != nil {
			return fmt.Errorf("stat %s: %w", filepath.Base(path), err)
		}
		return nil
	}
	final := path + machine.CompressedSuffix
	tmp := final + ".tmp"
	if err := os.Remove(tmp); err != nil && !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("remove stale %s: %w", filepath.Base(tmp), err)
	}
	dst, err := os.OpenFile(tmp, os.O_WRONLY|os.O_CREATE|os.O_EXCL|syscall.O_NOFOLLOW, 0644)
	if err != nil {
		return fmt.Errorf("create %s: %w", filepath.Base(tmp), err)
	}
	zw := gzip.NewWriter(dst)
	_, err = io.Copy(zw, src)
	if err == nil {
		err = zw.Close()
	}
	if err == nil {
		err = dst.Sync()
	}
	if closeErr := dst.Close(); err == nil {
		err = closeErr
	}
	if err == nil {
		err = os.Rename(tmp, final)
	}
	if err != nil {
		_ = os.Remove(tmp)
		return fmt.Errorf("compress %s: %w", filepath.Base(path), err)
	}
	dir := filepath.Dir(path)
	if err := syncDirs(dir); err != nil {
		return fmt.Errorf("sync compressed %s: %w", filepath.Base(path), err)
	}
	if err := os.Remove(path); err != nil {
		return fmt.Errorf("remove plain %s: %w", filepath.Base(path), err)
	}
	if err := syncDirs(dir); err != nil {
		return fmt.Errorf("sync removal of %s: %w", filepath.Base(path), err)
	}
	return nil
}

func syncDirs(paths ...string) error {
	for _, path := range paths {
		d, err := os.Open(path)
		if err == nil {
			err = d.Sync()
			closeErr := d.Close()
			if err == nil {
				err = closeErr
			}
		}
		if err != nil {
			return err
		}
	}
	return nil
}

// compressBackendLogs compresses every instance's stdout.log and stderr.log once the stream readers have closed them.
func (t *running) compressBackendLogs() error {
	var errs []error
	for _, inst := range t.instances {
		for _, name := range []string{"stdout.log", "stderr.log"} {
			errs = append(errs, compressTrialFile(filepath.Join(inst.dir, name)))
		}
	}
	return errors.Join(errs...)
}
