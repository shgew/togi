package journal

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

func (j *Journal) SessionID(now time.Time) (string, error) {
	base := now.UTC().Format("20060102T150405Z")
	boundary, err := ResetBoundary(j.dir)
	if err != nil {
		return "", err
	}
	suffix := 1
	if boundary != "" && CompareSessionIDs(base, boundary) <= 0 {
		base, suffix = sessionSuffix(boundary)
		suffix++
	}
	for ; ; suffix++ {
		id := base
		if suffix > 1 {
			id += "-" + strconv.Itoa(suffix)
		}
		exists, err := j.archiveExists(id)
		if err != nil {
			return "", err
		}
		if !exists {
			return id, nil
		}
	}
}

func (j *Journal) archiveExists(session string) (bool, error) {
	for _, name := range []string{session + ".jsonl", session + trialsSuffix} {
		path := filepath.Join(j.dir, archiveDir, name)
		if _, err := os.Stat(path); err == nil {
			return true, nil
		} else if !errors.Is(err, fs.ErrNotExist) {
			return false, fmt.Errorf("check archive %s: %w", path, err)
		}
	}
	return false, nil
}

func CompareSessionIDs(a, b string) int {
	aBase, aSuffix := sessionSuffix(a)
	bBase, bSuffix := sessionSuffix(b)
	if order := strings.Compare(aBase, bBase); order != 0 {
		return order
	}
	if aSuffix < bSuffix {
		return -1
	}
	if aSuffix > bSuffix {
		return 1
	}
	return strings.Compare(a, b)
}

func sessionSuffix(id string) (string, int) {
	if at := strings.LastIndexByte(id, '-'); at > 0 {
		if suffix, err := strconv.Atoi(id[at+1:]); err == nil && suffix > 1 {
			return id[:at], suffix
		}
	}
	return id, 1
}
