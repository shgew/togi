// Package simrun drives a session on the simulator: a crash reboots the simulated machine and the next boot resumes
// the journal, until the session stops.
package simrun

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"path/filepath"

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
	// Cycles is the number of clean cycles of one profile after which the run stops; 0 keeps checking endlessly.
	Cycles int
	Wrap   func(session.Journal) session.Journal
	Until  func(journal.Event) bool
	// InMemoryJournal retains the writer across simulated reboots and writes state only when Simulate returns.
	// Leave it false when testing file recovery or injecting journal interruptions.
	InMemoryJournal bool
	WriteSamples    bool
}

func Simulate(ctx context.Context, in Input) (stop session.Stop, err error) {
	var cached *memoryJournal
	prefix := &journal.Prefix{}
	defer func() {
		if cached != nil {
			err = errors.Join(err, cached.flush(), cached.Close())
		}
	}()
	samplesDir := ""
	if in.WriteSamples {
		samplesDir = filepath.Join(in.Dir, "trials")
	}
	in.Machine.SetSamplesDir(samplesDir)
	for range maxBoots {
		stop, err = boot(ctx, in, &cached, prefix)
		if errors.Is(err, machine.ErrCrashed) {
			in.Machine.Reboot()
			continue
		}
		return stop, err
	}
	return session.Stop{}, fmt.Errorf("simulated machine rebooted %d times without stopping", maxBoots)
}

func boot(ctx context.Context, in Input, cached **memoryJournal, prefix *journal.Prefix) (session.Stop, error) {
	seams := in.Machine.Seams()
	id, err := seams.Host.BootID()
	if err != nil {
		return session.Stop{}, fmt.Errorf("read boot id: %w", err)
	}
	current, err := seams.Host.BIOSContext()
	if err != nil {
		return session.Stop{}, fmt.Errorf("read BIOS context: %w", err)
	}
	var j *journal.Journal
	var carried *carry.Carry
	if *cached != nil {
		j = (*cached).Journal
		j.SetBoot(id)
		carried = (*cached).carried
	} else {
		j, err = journal.Lock(in.Dir, journal.Options{Boot: id, Now: in.Machine.Now, Monotonic: seams.Clock.Monotonic, Log: in.Log, Renderer: in.Renderer, Build: session.Build(), Prefix: prefix})
		if err != nil {
			return session.Stop{}, err
		}
		defer func() {
			if *cached == nil {
				_ = j.Close()
			}
		}()
		carried, err = carry.Prepare(j, session.Build(), nil, &current)
		if err != nil {
			return session.Stop{}, err
		}
		if err := j.Open(); err != nil {
			return session.Stop{}, err
		}
		if in.InMemoryJournal {
			*cached = &memoryJournal{Journal: j, carried: carried}
		}
	}
	runCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	var wrapped session.Journal = j
	if *cached != nil {
		wrapped = *cached
	}
	if in.Until != nil {
		wrapped = &untilJournal{Journal: wrapped, until: in.Until, cancel: cancel}
	}
	if in.Wrap != nil {
		wrapped = in.Wrap(wrapped)
	}
	stop, err := session.Run(runCtx, session.Input{Config: in.Config, ConfigPath: in.ConfigPath, Boot: id, Journal: wrapped, Machine: seams, Cycles: in.Cycles, Carry: carried, Stderr: in.Log, SessionID: j.SessionID})
	if *cached != nil {
		if errors.Is(err, machine.ErrCrashed) {
			if serr := (*cached).snapshot(); serr != nil {
				return session.Stop{}, serr
			}
		}
	} else if cerr := j.Close(); err == nil && cerr != nil {
		return session.Stop{}, cerr
	}
	return stop, err
}

type memoryJournal struct {
	*journal.Journal
	carried  *carry.Carry
	state    journal.State
	hasState bool
}

func (j *memoryJournal) WriteState(state journal.State) error {
	j.state = state
	j.hasState = true
	return nil
}

func (j *memoryJournal) ReadState() (journal.State, error) {
	if !j.hasState {
		return j.Journal.ReadState()
	}
	return j.state, nil
}

func (j *memoryJournal) snapshot() error {
	// Match the persisted projection's JSON representation and detach slices and pointers before replaying the next boot.
	if !j.hasState {
		return nil
	}
	data, err := json.Marshal(j.state)
	if err != nil {
		return fmt.Errorf("snapshot simulated state: %w", err)
	}
	var state journal.State
	if err := json.Unmarshal(data, &state); err != nil {
		return fmt.Errorf("snapshot simulated state: %w", err)
	}
	j.state = state
	return nil
}

func (j *memoryJournal) flush() error {
	if !j.hasState {
		return nil
	}
	if err := j.Journal.WriteState(j.state); err != nil {
		var cause []int
		if j.state.LastSeq > 0 {
			cause = []int{j.state.LastSeq}
		}
		_, warningErr := j.Append(&journal.SessionWarning{Operation: "write state projection", Error: err.Error()}, cause...)
		return warningErr
	}
	return nil
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
