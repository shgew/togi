package session

import (
	"errors"
	"fmt"
	"slices"

	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/tuner"
)

var (
	ErrRegainPending   = errors.New("a regain is already queued or running; shycler run finishes it")
	ErrNothingToRegain = errors.New("no unproven depth to regain")
	ErrNoSession       = errors.New("no session in the journal")
)

type replayed struct {
	state journal.State
	tuner *tuner.State
}

func replayFor(j Journal) (replayed, error) {
	r := replayed{tuner: tuner.New()}
	events := j.Events()
	if len(events) == 0 {
		return r, ErrNoSession
	}
	if err := journal.Compatible(journal.BuildOf(events), Build()); err != nil {
		return r, err
	}
	journal.Replay(events, &r.state, r.tuner)
	r.tuner.Project(&r.state)
	return r, nil
}

func (r replayed) hasCore(core int) bool {
	return slices.ContainsFunc(r.state.Cores, func(c journal.CoreState) bool { return c.Core == core })
}

// record appends a command and the shutdown that keeps its boot from reading as a crash, then writes the state.
func (r replayed) record(j Journal, p journal.Payload) error {
	cmd, err := j.Append(p)
	if err != nil {
		return err
	}
	stop, err := j.Append(&journal.Shutdown{Reason: journal.ShutdownCommand}, cmd.Seq)
	if err != nil {
		return err
	}
	journal.Replay([]journal.Event{cmd, stop}, &r.state, r.tuner)
	r.tuner.Project(&r.state)
	return j.WriteState(r.state)
}

// Regain queues one count of regain on each eligible core, or only on core when it is given.
func Regain(j Journal, core *int) ([]int, error) {
	r, err := replayFor(j)
	if err != nil {
		return nil, err
	}
	if r.tuner.RegainPending() {
		return nil, ErrRegainPending
	}
	cores := r.tuner.Regainable()
	if core != nil {
		if !r.hasCore(*core) {
			return nil, fmt.Errorf("regain core %d: %w", *core, ErrNoSuchCore)
		}
		if !slices.Contains(cores, *core) {
			return nil, fmt.Errorf("core %02d: %w", *core, ErrNothingToRegain)
		}
		cores = []int{*core}
	}
	if len(cores) == 0 {
		return nil, ErrNothingToRegain
	}
	if err := r.record(j, &journal.CommandRegain{Cores: cores}); err != nil {
		return nil, fmt.Errorf("queue regain: %w", err)
	}
	return cores, nil
}

func ResetCore(j Journal, core int) error {
	r, err := replayFor(j)
	if err != nil {
		return err
	}
	if !r.hasCore(core) {
		return fmt.Errorf("reset core %d: %w", core, ErrNoSuchCore)
	}
	if err := r.record(j, &journal.CommandReset{Core: new(core)}); err != nil {
		return fmt.Errorf("queue reset of core %d: %w", core, err)
	}
	return nil
}

// ResetAll records the reset and archives the session; the journal is spent afterwards.
func ResetAll(j *journal.Journal) (string, error) {
	events := j.Events()
	if len(events) == 0 {
		return "", ErrNoSession
	}
	session := events[0].Data.(*journal.SessionStart).Session
	if _, err := j.ArchivePath(session); err != nil {
		return "", err
	}
	if _, err := j.Append(&journal.CommandReset{All: true}); err != nil {
		return "", fmt.Errorf("reset: %w", err)
	}
	return j.Archive(session)
}
