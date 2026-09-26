package sim

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"time"

	"github.com/shgew/shycler/internal/journal"
)

// Resume continues the simulated machine after the journal in dir and its archives: their boots are counted and the
// clock starts after their last event, so a later simulated run never reuses a boot ID, a session ID or rewinds time.
func Resume(dir string, cfg Config) (Config, error) {
	archives, err := filepath.Glob(filepath.Join(dir, "archive", "*.jsonl"))
	if err != nil {
		return cfg, fmt.Errorf("resume simulator: %w", err)
	}
	boots := map[string]bool{}
	var last time.Time
	for _, path := range append(archives, filepath.Join(dir, "events.jsonl")) {
		events, _, err := journal.ReadFile(path)
		if errors.Is(err, fs.ErrNotExist) {
			continue
		}
		var incompatible *journal.IncompatibleError
		if path != filepath.Join(dir, "events.jsonl") && errors.As(err, &incompatible) && incompatible.Field == "schema" {
			if err := resumeArchiveMetadata(path, boots, &last); err != nil {
				return cfg, fmt.Errorf("resume simulator: %w", err)
			}
			continue
		}
		if err != nil {
			return cfg, fmt.Errorf("resume simulator: %w", err)
		}
		for _, e := range events {
			boots[e.Boot] = true
			if e.Time.After(last) {
				last = e.Time
			}
		}
	}
	if len(boots) == 0 {
		return cfg, nil
	}
	cfg.Boots = len(boots)
	cfg.Start = last.Add(RebootTime)
	return cfg, nil
}

func resumeArchiveMetadata(path string, boots map[string]bool, last *time.Time) error {
	data, err := os.ReadFile(path)
	if err != nil {
		return fmt.Errorf("read archived journal %s: %w", path, err)
	}
	for len(data) > 0 {
		end := bytes.IndexByte(data, '\n')
		if end < 0 {
			break
		}
		var event struct {
			Boot string    `json:"boot"`
			Time time.Time `json:"time"`
		}
		if err := json.Unmarshal(data[:end], &event); err != nil {
			return fmt.Errorf("read archived journal %s metadata: %w", path, err)
		}
		boots[event.Boot] = true
		if event.Time.After(*last) {
			*last = event.Time
		}
		data = data[end+1:]
	}
	return nil
}
