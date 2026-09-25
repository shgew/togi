package session

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"time"

	"code.marleb.org/shgew/shycler/internal/config"
	"code.marleb.org/shgew/shycler/internal/defect"
	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
	"code.marleb.org/shgew/shycler/internal/sim"
)

const maxSimulatedBoots = 1000

type SimInput struct {
	Config     config.Config
	ConfigPath string
	ConfigFile bool
	Dir        string
	Machine    *sim.Machine
	Log        io.Writer
	Renderer   journal.Renderer
	// Rotations is the number of clean rotations of one profile after which the run stops; 0 runs guard endlessly.
	Rotations  int
	Bootloader Bootloader
	Prompt     func(defect.Finding) (bool, error)
	Defects    []defect.Entry
}

// Resume continues the simulated machine after the journal in dir and its archives: their boots are counted and the
// clock starts after their last event, so a later simulated run never reuses a boot ID, a session ID or rewinds time.
func Resume(dir string, cfg sim.Config) (sim.Config, error) {
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
	cfg.Start = last.Add(sim.RebootTime)
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

func Simulate(ctx context.Context, in SimInput) (Stop, error) {
	for range maxSimulatedBoots {
		stop, err := simulateBoot(ctx, in, nil)
		if errors.Is(err, machine.ErrCrashed) {
			in.Machine.Reboot()
			continue
		}
		return stop, err
	}
	return Stop{}, fmt.Errorf("simulated machine rebooted %d times without stopping", maxSimulatedBoots)
}

func simulateBoot(ctx context.Context, in SimInput, wrap func(*journal.Journal) Journal) (Stop, error) {
	m := in.Machine
	seams := m.Seams()
	boot, err := seams.Host.BootID()
	if err != nil {
		return Stop{}, fmt.Errorf("read boot id: %w", err)
	}
	j, err := journal.Open(in.Dir, journal.Options{Boot: boot, Now: m.Now, Log: in.Log, Renderer: in.Renderer, Build: Build()})
	if err != nil {
		return Stop{}, err
	}
	var jr Journal = j
	if wrap != nil {
		jr = wrap(j)
	}
	stop, err := Run(ctx, Input{Config: in.Config, ConfigPath: in.ConfigPath, ConfigFile: in.ConfigFile, Boot: boot, Journal: jr, Machine: seams, Rotations: in.Rotations, Bootloader: in.Bootloader, Prompt: in.Prompt, Defects: in.Defects})
	if cerr := j.Close(); err == nil && cerr != nil {
		return Stop{}, cerr
	}
	return stop, err
}
