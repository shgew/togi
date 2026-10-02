package journal

import (
	"io/fs"
	"os"
)

type journalFile interface {
	Write([]byte) (int, error)
	Sync() error
	Truncate(int64) error
	Close() error
}

type journalFilesystem interface {
	OpenFile(string, int, fs.FileMode) (journalFile, error)
	Rename(string, string) error
	Remove(string) error
	SyncDir(string) error
}

type diskJournalFilesystem struct{}

func (diskJournalFilesystem) OpenFile(path string, flags int, mode fs.FileMode) (journalFile, error) {
	return os.OpenFile(path, flags, mode)
}

func (diskJournalFilesystem) Rename(from, to string) error { return os.Rename(from, to) }
func (diskJournalFilesystem) Remove(path string) error     { return os.Remove(path) }
func (diskJournalFilesystem) SyncDir(path string) error    { return syncDir(path) }
