package session

import (
	"errors"
	"fmt"
	"slices"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/tuner"
)

var ErrNoSession = errors.New("no session in the journal")

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
	if err := j.MarkResetAll(); err != nil {
		return "", fmt.Errorf("reset: %w", err)
	}
	if _, err := j.Append(&journal.CommandReset{All: true}); err != nil {
		return "", fmt.Errorf("reset: %w", err)
	}
	return j.Archive(session)
}
