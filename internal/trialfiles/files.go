// Package trialfiles preserves completed trials' samples and backend logs in gzip form and opens either stored form.
package trialfiles

import (
	"compress/gzip"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"syscall"
)

// Samples names a trial's conditions samples.
const Samples = "samples.jsonl"

// Compress finishes or retries compression after all writers in dir have stopped.
// Missing files and directories are harmless: a trial may have stopped before creating them.
func Compress(dir string) error {
	entries, err := os.ReadDir(dir)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return fmt.Errorf("list trial files: %w", err)
	}
	errs := []error{compressFile(filepath.Join(dir, Samples), syncDir)}
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		for _, name := range []string{"stdout.log", "stderr.log"} {
			errs = append(errs, compressFile(filepath.Join(dir, entry.Name(), name), syncDir))
		}
	}
	return errors.Join(errs...)
}

// compressFile replaces the regular file at path with its gzip form at path+".gz". The compressed file is written
// whole under a temporary name, synced and renamed into place before the plain file is removed, so a reader finds one
// complete form at every moment and a crash leaves either both (readers prefer the plain one) or the compressed one.
// A missing path is skipped. So is anything that is not a regular file: a workload may replace its logs with a
// symlink, which is never followed. syncDir makes a change to path's directory durable.
func compressFile(path string, syncDir func(string) error) error {
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
	final := path + ".gz"
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
	if err := syncDir(dir); err != nil {
		return fmt.Errorf("sync compressed %s: %w", filepath.Base(path), err)
	}
	if err := os.Remove(path); err != nil {
		return fmt.Errorf("remove plain %s: %w", filepath.Base(path), err)
	}
	if err := syncDir(dir); err != nil {
		return fmt.Errorf("sync removal of %s: %w", filepath.Base(path), err)
	}
	return nil
}

func syncDir(path string) error {
	d, err := os.Open(path)
	if err != nil {
		return err
	}
	err = d.Sync()
	if closeErr := d.Close(); err == nil {
		err = closeErr
	}
	return err
}

// Open opens the trial file at path, or its compressed form when the plain one is gone. Compression removes the
// plain file only after the compressed one is complete, so one of the two is always readable.
func Open(path string) (io.ReadCloser, error) {
	f, err := os.Open(path)
	if err == nil {
		return f, nil
	}
	if !errors.Is(err, os.ErrNotExist) {
		return nil, err
	}
	f, err = os.Open(path + ".gz")
	if err != nil {
		return nil, err
	}
	z, err := gzip.NewReader(f)
	if err != nil {
		f.Close()
		return nil, err
	}
	return compressedFile{Reader: z, file: f}, nil
}

type compressedFile struct {
	*gzip.Reader
	file *os.File
}

func (c compressedFile) Close() error {
	return errors.Join(c.Reader.Close(), c.file.Close())
}
