package journal

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"syscall"
)

const resetAllSuffix = "-reset-all"

// ResetBoundary returns the newest session discarded by reset --all.
// The marker is immutable and independent of the journal schema.
func ResetBoundary(dir string) (string, error) {
	entries, err := os.ReadDir(filepath.Join(dir, archiveDir))
	if errors.Is(err, fs.ErrNotExist) {
		return "", nil
	}
	if err != nil {
		return "", fmt.Errorf("read reset boundaries: %w", err)
	}
	var boundary string
	for _, entry := range entries {
		if id, ok := strings.CutSuffix(entry.Name(), resetAllSuffix); ok {
			if id == "" || !entry.Type().IsRegular() {
				return "", fmt.Errorf("invalid reset boundary %s", entry.Name())
			}
			if CompareSessionIDs(id, boundary) > 0 {
				boundary = id
			}
		}
	}
	return boundary, nil
}

// MarkResetAll durably cuts carry lineage before reset --all mutates its sources.
// The caller holds the state-directory writer lock.
func (j *Journal) MarkResetAll() error {
	_, boundary, err := Scan(j.dir)
	if err != nil && !errors.Is(err, fs.ErrNotExist) {
		return fmt.Errorf("scan reset boundary: %w", err)
	}
	archive := filepath.Join(j.dir, archiveDir)
	entries, err := os.ReadDir(archive)
	if err != nil && !errors.Is(err, fs.ErrNotExist) {
		return fmt.Errorf("read reset sources: %w", err)
	}
	for _, entry := range entries {
		for _, suffix := range []string{".jsonl", carrySuffix, "-compat-pending", resetAllSuffix} {
			if id, ok := strings.CutSuffix(entry.Name(), suffix); ok && CompareSessionIDs(id, boundary) > 0 {
				boundary = id
			}
		}
	}
	if boundary == "" {
		return nil
	}
	if err := os.MkdirAll(archive, 0o755); err != nil {
		return fmt.Errorf("create reset boundary directory: %w", err)
	}
	marker, err := j.fs.OpenFile(filepath.Join(archive, boundary+resetAllSuffix), os.O_CREATE|os.O_WRONLY|syscall.O_NOFOLLOW|syscall.O_NONBLOCK, 0o644)
	if err != nil {
		return fmt.Errorf("mark reset boundary: %w", err)
	}
	if j.opts.Sync {
		if err := marker.Sync(); err != nil {
			_ = marker.Close()
			return fmt.Errorf("sync reset boundary: %w", err)
		}
	}
	if err := marker.Close(); err != nil {
		return fmt.Errorf("close reset boundary: %w", err)
	}
	if j.opts.Sync {
		for _, dir := range []string{archive, j.dir} {
			if err := j.fs.SyncDir(dir); err != nil {
				return fmt.Errorf("sync reset boundary directory: %w", err)
			}
		}
	}
	return nil
}
