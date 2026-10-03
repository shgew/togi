package detect

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"path"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/shgew/togi/internal/machine"
)

const pstoreDir = "/var/lib/systemd/pstore"
const pstoreTailBytes = 4096
const pstoreTailLines = 32

func (k *Kernel) SavedPstore(boot string) (*machine.PstoreRecord, error) {
	entries, err := fs.ReadDir(k.pstore, ".")
	if errors.Is(err, fs.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("list pstore archive: %w", err)
	}
	if len(entries) == 0 {
		return nil, nil
	}
	start, end, err := k.pstoreBootSpan(boot)
	if err != nil {
		return nil, err
	}
	selected := ""
	latest := int64(0)
	for _, entry := range entries {
		stamp, err := strconv.ParseInt(entry.Name(), 10, 64)
		if err != nil || !entry.IsDir() || stamp <= 0 {
			continue
		}
		when := time.Unix(stamp, 0)
		if when.Before(start) || !when.Before(end) {
			continue
		}
		counts, err := fs.ReadDir(k.pstore, entry.Name())
		if err != nil {
			return nil, fmt.Errorf("list pstore record %s: %w", entry.Name(), err)
		}
		for _, count := range counts {
			if len(count.Name()) != 3 || !count.IsDir() {
				continue
			}
			if _, err := strconv.ParseUint(count.Name(), 10, 16); err != nil {
				continue
			}
			file := path.Join(entry.Name(), count.Name(), "dmesg.txt")
			info, err := fs.Stat(k.pstore, file)
			if errors.Is(err, fs.ErrNotExist) {
				continue
			}
			if err != nil {
				return nil, fmt.Errorf("stat pstore record %s: %w", file, err)
			}
			if !info.Mode().IsRegular() {
				return nil, fmt.Errorf("stat pstore record %s: not a regular file", file)
			}
			if stamp > latest || stamp == latest && file > selected {
				selected, latest = file, stamp
			}
		}
	}
	if selected == "" {
		return nil, nil
	}
	file, err := k.pstore.Open(selected)
	if err != nil {
		return nil, fmt.Errorf("open pstore record %s: %w", selected, err)
	}
	lines, readErr := pstoreTail(file)
	closeErr := file.Close()
	if err := errors.Join(readErr, closeErr); err != nil {
		return nil, fmt.Errorf("read pstore record %s: %w", selected, err)
	}
	return &machine.PstoreRecord{Path: path.Join(pstoreDir, selected), Lines: lines}, nil
}

func (k *Kernel) pstoreBootSpan(boot string) (time.Time, time.Time, error) {
	out, stderr, code, err := k.journalctl([]string{"--list-boots", "-o", "json", "--no-pager", "-q"})
	if err != nil {
		return time.Time{}, time.Time{}, fmt.Errorf("list system boots for pstore %s: %w: %s", boot, err, bytes.TrimSpace(stderr))
	}
	if code != 0 {
		return time.Time{}, time.Time{}, fmt.Errorf("list system boots for pstore %s: exit status %d: %s", boot, code, bytes.TrimSpace(stderr))
	}
	var boots []struct {
		ID    string `json:"boot_id"`
		First int64  `json:"first_entry"`
	}
	if err := json.Unmarshal(out, &boots); err != nil {
		return time.Time{}, time.Time{}, fmt.Errorf("decode system boots for pstore %s: %w", boot, err)
	}
	for i, b := range boots {
		if b.ID == strings.ReplaceAll(boot, "-", "") && i+1 < len(boots) && b.First > 0 && boots[i+1].First > b.First {
			return time.UnixMicro(b.First), time.UnixMicro(boots[i+1].First), nil
		}
	}
	return time.Time{}, time.Time{}, nil
}

func pstoreTail(file fs.File) ([]string, error) {
	info, err := file.Stat()
	if err != nil {
		return nil, fmt.Errorf("stat opened record: %w", err)
	}
	if !info.Mode().IsRegular() {
		return nil, errors.New("opened record is not a regular file")
	}
	reader, ok := file.(io.ReaderAt)
	if !ok {
		return nil, errors.New("opened record does not support bounded reads")
	}
	size := min(info.Size(), int64(pstoreTailBytes))
	data := make([]byte, size)
	if _, err := reader.ReadAt(data, info.Size()-size); err != nil {
		return nil, err
	}
	if info.Size() > size {
		if i := bytes.IndexByte(data, '\n'); i >= 0 {
			data = data[i+1:]
		} else {
			for len(data) > 0 && !utf8.RuneStart(data[0]) {
				data = data[1:]
			}
		}
	}
	lines := strings.Split(strings.TrimSuffix(string(data), "\n"), "\n")
	return lines[max(0, len(lines)-pstoreTailLines):], nil
}
