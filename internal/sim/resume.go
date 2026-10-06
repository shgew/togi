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

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

// Resume continues the simulated machine after the journal in dir and its archives: their boots are counted and the
// clock starts after their last event, so a later simulated run never reuses a boot ID, a session ID or rewinds time.
// Without a configured BIOS context, the machine reports the newest one they recorded.
func Resume(dir string, cfg Config) (Config, error) {
	sessions, err := journal.ArchivedSessions(dir)
	if err != nil {
		return cfg, fmt.Errorf("resume simulator: %w", err)
	}
	var archives []string
	for _, id := range sessions {
		archives = append(archives, filepath.Join(dir, "archive", id+".jsonl"))
	}
	boots := map[string]bool{}
	var (
		last time.Time
		bios *machine.BIOSContext
	)
	for _, path := range append(archives, filepath.Join(dir, "events.jsonl")) {
		events, _, err := journal.ReadFile(path)
		if errors.Is(err, fs.ErrNotExist) {
			continue
		}
		var incompatible *journal.IncompatibleError
		if errors.As(err, &incompatible) && incompatible.Field == "schema" {
			if err := resumeArchiveMetadata(path, boots, &last, &bios); err != nil {
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
			if p, ok := e.Data.(*journal.SessionContext); ok {
				bios = &p.BIOSContext
			}
		}
	}
	if bios != nil && cfg.BIOSContext == (machine.BIOSContext{}) {
		cfg.BIOSContext = *bios
	}
	if len(boots) == 0 {
		return cfg, nil
	}
	cfg.Boots = len(boots)
	cfg.Start = last.Add(RebootTime)
	return cfg, nil
}

func resumeArchiveMetadata(path string, boots map[string]bool, last *time.Time, bios **machine.BIOSContext) error {
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
			Boot string       `json:"boot"`
			Time time.Time    `json:"time"`
			Kind journal.Kind `json:"kind"`
		}
		if err := json.Unmarshal(data[:end], &event); err != nil {
			return fmt.Errorf("read archived journal %s metadata: %w", path, err)
		}
		boots[event.Boot] = true
		if event.Time.After(*last) {
			*last = event.Time
		}
		if event.Kind == journal.KindSessionContext {
			var ctx journal.SessionContext
			if err := json.Unmarshal(data[:end], &ctx); err != nil {
				return fmt.Errorf("read archived journal %s context: %w", path, err)
			}
			*bios = &ctx.BIOSContext
		}
		data = data[end+1:]
	}
	return nil
}
