// Package simrun drives a session on the simulator: a crash reboots the simulated machine and the next boot resumes
// the journal, until the session stops.
package simrun

import (
	"context"
	"errors"
	"fmt"
	"io"

	"code.marleb.org/shgew/shycler/internal/config"
	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
	"code.marleb.org/shgew/shycler/internal/session"
	"code.marleb.org/shgew/shycler/internal/sim"
)

const maxBoots = 1000

type Input struct {
	Config     config.Config
	ConfigPath string
	Dir        string
	Machine    *sim.Machine
	Log        io.Writer
	Renderer   journal.Renderer
	// Rotations is the number of clean rotations of one profile after which the run stops; 0 runs guard endlessly.
	Rotations int
}

func Simulate(ctx context.Context, in Input) (session.Stop, error) {
	for range maxBoots {
		stop, err := boot(ctx, in)
		if errors.Is(err, machine.ErrCrashed) {
			in.Machine.Reboot()
			continue
		}
		return stop, err
	}
	return session.Stop{}, fmt.Errorf("simulated machine rebooted %d times without stopping", maxBoots)
}

func boot(ctx context.Context, in Input) (session.Stop, error) {
	seams := in.Machine.Seams()
	id, err := seams.Host.BootID()
	if err != nil {
		return session.Stop{}, fmt.Errorf("read boot id: %w", err)
	}
	j, err := journal.Open(in.Dir, journal.Options{Boot: id, Now: in.Machine.Now, Log: in.Log, Renderer: in.Renderer, Build: session.Build()})
	if err != nil {
		return session.Stop{}, err
	}
	stop, err := session.Run(ctx, session.Input{Config: in.Config, ConfigPath: in.ConfigPath, Boot: id, Journal: j, Machine: seams, Rotations: in.Rotations})
	if cerr := j.Close(); err == nil && cerr != nil {
		return session.Stop{}, cerr
	}
	return stop, err
}
