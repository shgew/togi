package journal

import (
	"bufio"
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"slices"
	"strings"
)

// BootReasonRecorded includes archives: a transition may move the acknowledgement
// after a crash, before the next run can clear the pending GRUB record.
func (j *Journal) BootReasonRecorded(id string) (bool, error) {
	if slices.ContainsFunc(j.events, func(e Event) bool {
		p, ok := e.Data.(*BootLeaveReason)
		return ok && p.ReasonID == id
	}) {
		return true, nil
	}
	archive := filepath.Join(j.dir, archiveDir)
	entries, err := os.ReadDir(archive)
	if errors.Is(err, fs.ErrNotExist) {
		return false, nil
	}
	if err != nil {
		return false, fmt.Errorf("read archived boot reasons: %w", err)
	}
	for _, entry := range slices.Backward(entries) {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".jsonl") {
			continue
		}
		found, err := archivedBootReason(filepath.Join(archive, entry.Name()), id)
		if found || err != nil {
			return found, err
		}
	}
	return false, nil
}

func archivedBootReason(path, id string) (bool, error) {
	file, err := os.Open(path)
	if err != nil {
		return false, fmt.Errorf("read archived boot reason %s: %w", path, err)
	}
	defer file.Close()
	reader := bufio.NewReader(file)
	var continued []byte
	kind := []byte(KindBootLeaveReason)
	for {
		line, err := reader.ReadSlice('\n')
		if errors.Is(err, bufio.ErrBufferFull) {
			continued = append(continued, line...)
			continue
		}
		if errors.Is(err, io.EOF) {
			return false, nil
		}
		if err != nil {
			return false, fmt.Errorf("read archived boot reason %s: %w", path, err)
		}
		if len(continued) > 0 {
			line = append(continued, line...)
			continued = line[:0]
		}
		if !bytes.Contains(line, kind) {
			continue
		}
		var record struct {
			Kind     Kind   `json:"kind"`
			ReasonID string `json:"reason_id"`
		}
		if err := json.Unmarshal(line, &record); err != nil {
			return false, fmt.Errorf("decode archived boot reason %s: %w", path, err)
		}
		if record.Kind == KindBootLeaveReason && record.ReasonID == id {
			return true, nil
		}
	}
}
