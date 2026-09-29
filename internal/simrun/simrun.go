// Package simrun drives a session on the simulator: a crash reboots the simulated machine and the next boot resumes
// the journal, until the session stops.
package simrun

import (
	"context"
	"errors"
	"fmt"
	"io"

	"github.com/shgew/togi/internal/carry"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
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
	Wrap       func(session.Journal) session.Journal
	Until      func(journal.Event) bool
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
	current, err := seams.Host.BIOSContext()
	if err != nil {
		return session.Stop{}, fmt.Errorf("read BIOS context: %w", err)
	}
	carried, err := carry.Prepare(in.Dir, journal.Options{Boot: id, Now: in.Machine.Now}, session.Build(), nil, &current)
	if err != nil {
		return session.Stop{}, err
	}
	j, err := journal.Open(in.Dir, journal.Options{Boot: id, Now: in.Machine.Now, Monotonic: seams.Clock.Monotonic, Log: in.Log, Renderer: in.Renderer, Build: session.Build()})
	if err != nil {
		return session.Stop{}, err
	}
	runCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	var wrapped session.Journal = j
	if in.Until != nil {
		wrapped = &untilJournal{Journal: wrapped, until: in.Until, cancel: cancel}
	}
	if in.Wrap != nil {
		wrapped = in.Wrap(wrapped)
	}
	stop, err := session.Run(runCtx, session.Input{Config: in.Config, ConfigPath: in.ConfigPath, Boot: id, Journal: wrapped, Machine: seams, Rotations: in.Rotations, Carry: carried, Stderr: in.Log})
	if cerr := j.Close(); err == nil && cerr != nil {
		return session.Stop{}, cerr
	}
	return stop, err
}

type untilJournal struct {
	session.Journal
	until  func(journal.Event) bool
	cancel context.CancelFunc
	done   bool
}

func (j *untilJournal) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := j.Journal.Append(p, cause...)
	if err == nil && !j.done && j.until(e) {
		j.done = true
		j.cancel()
	}
	return e, err
}
