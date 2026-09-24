package session

import (
	"context"
	"errors"
	"fmt"
	"io"
	"io/fs"

	"code.marleb.org/shgew/shycler/internal/config"
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
	// Rotations is the number of clean rotations of one profile after which the run stops; 0 runs guard endlessly.
	Rotations int
}

// Resume continues the simulated machine after the journal in dir: its boots are counted and its clock starts after
// the last event, so a later simulated run never reuses a boot ID or rewinds time.
func Resume(dir string, cfg sim.Config) (sim.Config, error) {
	events, _, err := journal.Read(dir)
	if errors.Is(err, fs.ErrNotExist) {
		return cfg, nil
	}
	if err != nil {
		return cfg, fmt.Errorf("resume simulator: %w", err)
	}
	boots := map[string]bool{}
	for _, e := range events {
		boots[e.Boot] = true
	}
	cfg.Boots = len(boots)
	if n := len(events); n > 0 {
		cfg.Start = events[n-1].Time.Add(sim.RebootTime)
	}
	return cfg, nil
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
	j, err := journal.Open(in.Dir, journal.Options{Boot: boot, Now: m.Now, Log: in.Log})
	if err != nil {
		return Stop{}, err
	}
	var jr Journal = j
	if wrap != nil {
		jr = wrap(j)
	}
	stop, err := Run(ctx, Input{Config: in.Config, ConfigPath: in.ConfigPath, ConfigFile: in.ConfigFile, Boot: boot, Journal: jr, Machine: seams, Rotations: in.Rotations})
	if cerr := j.Close(); err == nil && cerr != nil {
		return Stop{}, cerr
	}
	return stop, err
}
