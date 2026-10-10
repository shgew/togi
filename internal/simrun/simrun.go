// Package simrun drives a session on the simulator: a crash reboots the simulated machine and the next boot resumes
// the journal, until the session stops.
package simrun

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"path/filepath"
	"slices"
	"time"

	"github.com/shgew/togi/internal/carry"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/render"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
)

const defaultMaxBoots = 1000

// ErrBootCap reports a session that was still running when the simulated machine reached its boot cap; the journal
// holds the partial session and state.json its projection. A capped run that then fails to finalize the journal
// returns an error that is not ErrBootCap.
var ErrBootCap = errors.New("simulated machine reached its boot cap without stopping")

// RecordedConfig returns the configuration a resume of dir runs under: the one the latest config.loaded in its journal
// recorded, standing in for the configuration file `togi run` would load. A journal of an older schema, which the
// resume archives, contributes only its recorded backend store paths on top of fresh. It returns fresh when dir holds
// no journal or no config.loaded.
func RecordedConfig(dir string, fresh config.Config) (config.Config, error) {
	recorded, _, err := journal.Scan(dir)
	if errors.Is(err, fs.ErrNotExist) {
		return fresh, nil
	}
	if err != nil {
		return config.Config{}, fmt.Errorf("read recorded configuration: %w", err)
	}
	if recorded.Schema < journal.Schema {
		return recordedBackends(dir, fresh)
	}
	events, _, err := journal.Read(dir)
	if err != nil {
		return config.Config{}, fmt.Errorf("read recorded configuration: %w", err)
	}
	for _, e := range slices.Backward(events) {
		if p, ok := e.Data.(*journal.ConfigLoaded); ok {
			return session.ConfigFromSnapshot(p.Config), nil
		}
	}
	return fresh, nil
}

func recordedBackends(dir string, fresh config.Config) (config.Config, error) {
	events, err := journal.ReadHistory(filepath.Join(dir, "events.jsonl"))
	if err != nil {
		return config.Config{}, fmt.Errorf("read recorded backends: %w", err)
	}
	for _, e := range slices.Backward(events) {
		if p, ok := e.Data.(*journal.ConfigLoaded); ok {
			fresh.Backends = config.Backends(p.Config.Backends)
			return fresh, nil
		}
	}
	return fresh, nil
}

type Input struct {
	Config     config.Config
	ConfigPath string
	Dir        string
	Machine    *sim.Machine
	Log        io.Writer
	Renderer   render.Renderer
	// Cycles is the number of clean cycles of one profile after which the run stops; 0 keeps checking endlessly.
	Cycles int
	Wrap   func(session.Journal) session.Journal
	Until  func(journal.Event) bool
	// InMemoryJournal retains the writer across simulated reboots and writes state only when Simulate returns.
	// Tests set it unless they inject journal interruptions; TestInMemoryJournalMatchesFileBacked pins that it leaves the
	// files a file-backed run does, so it is the file-mode coverage of state.json persistence.
	InMemoryJournal bool
	WriteSamples    bool
	// MaxBoots caps the simulated boots of one invocation; 0 uses 1000.
	MaxBoots int
	// ColdBoots replays the whole journal at every boot, as `togi run` does. Otherwise, with InMemoryJournal, a boot after a
	// simulated crash resumes from the state the crashed boot folded.
	ColdBoots bool
	// VerifyEvery makes every Nth boot that ends in a crash replay the whole journal into fresh state and fail unless it
	// equals the state the next boot would resume from; 0 verifies only when the session stops.
	VerifyEvery int
}

func Simulate(ctx context.Context, in Input) (stop session.Stop, err error) {
	journals := &journals{}
	var warm *session.Warm
	if in.InMemoryJournal && !in.ColdBoots {
		warm = &session.Warm{}
	}
	defer func() {
		kept := journals.kept
		if kept == nil {
			return
		}
		if state, ok := warm.TakePending(); ok {
			if writeErr := journals.last.WriteState(state); writeErr != nil {
				err = errors.Join(err, writeErr)
			}
		}
		finalizeErr := errors.Join(kept.flush(in.Log, in.Renderer), kept.Close())
		if finalizeErr != nil && errors.Is(err, ErrBootCap) {
			// Keep the cap in the message but out of the chain: an unfinalized capped run is an error, not censored.
			err = fmt.Errorf("%s; finalize simulated journal: %w", err.Error(), finalizeErr)
			return
		}
		err = errors.Join(err, finalizeErr)
	}()
	samplesDir := ""
	if in.WriteSamples {
		samplesDir = filepath.Join(in.Dir, "trials")
	}
	in.Machine.SetSamplesDir(samplesDir)
	maxBoots := in.MaxBoots
	if maxBoots == 0 {
		maxBoots = defaultMaxBoots
	}
	for n := 1; n <= maxBoots; n++ {
		stop, err = boot(ctx, in, journals, warm)
		crashed := errors.Is(err, machine.ErrCrashed)
		if warm != nil && !warm.Empty() && (err == nil || crashed) && (!crashed || in.VerifyEvery > 0 && n%in.VerifyEvery == 0) {
			if verifyErr := warm.Verify(journals.kept.Events()); verifyErr != nil {
				return session.Stop{}, fmt.Errorf("verify warm resume after boot %d: %w", n, verifyErr)
			}
		}
		if crashed {
			in.Machine.Reboot()
			continue
		}
		return stop, err
	}
	return session.Stop{}, fmt.Errorf("simulate session: %w after %d boots", ErrBootCap, maxBoots)
}

func boot(ctx context.Context, in Input, journals *journals, warm *session.Warm) (stop session.Stop, err error) {
	seams := in.Machine.Seams()
	id, err := seams.Host.BootID()
	if err != nil {
		return session.Stop{}, fmt.Errorf("read boot id: %w", err)
	}
	current, err := seams.Host.BIOSContext()
	if err != nil {
		return session.Stop{}, fmt.Errorf("read BIOS context: %w", err)
	}
	j, err := journals.open(in, id, current)
	if err != nil {
		return session.Stop{}, err
	}
	defer func() {
		if endErr := j.end(err); endErr != nil {
			stop, err = session.Stop{}, endErr
		}
	}()
	runCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	wrapped := j.Journal
	if in.Until != nil {
		wrapped = &untilJournal{Journal: wrapped, until: in.Until, cancel: cancel}
	}
	if in.Wrap != nil {
		wrapped = in.Wrap(wrapped)
	}
	journals.last = wrapped
	return session.Run(runCtx, session.Input{Config: in.Config, ConfigPath: in.ConfigPath, Boot: id, Journal: wrapped, Machine: seams, Cycles: in.Cycles, Carry: j.carried, Stderr: in.Log, Log: in.Log, Renderer: in.Renderer, SessionID: j.sessionID, Warm: warm, DeferState: in.InMemoryJournal})
}

// journals gives each boot its journal: one locked for the boot and closed when it ends or, with InMemoryJournal, one
// kept across boots that Simulate finalizes.
type journals struct {
	prefix journal.Prefix
	kept   *memoryJournal
	// last is the last boot's journal as the session saw it, which writes the state projection a crash left pending.
	last session.Journal
}

type bootJournal struct {
	session.Journal
	carried   *carry.Carry
	sessionID func(time.Time) (string, error)
	// end runs when the boot ends and returns the error that replaces the boot's result, if any.
	end func(runErr error) error
}

func (s *journals) open(in Input, id string, current machine.BIOSContext) (bootJournal, error) {
	if s.kept != nil {
		s.kept.SetBoot(id)
		return s.kept.boot(), nil
	}
	j, err := journal.Lock(in.Dir, journal.Options{Boot: id, Now: in.Machine.Now, Monotonic: in.Machine.Monotonic, Build: session.Build(), Prefix: &s.prefix})
	if err != nil {
		return bootJournal{}, err
	}
	carried, err := carry.Prepare(j, session.Build(), nil, &current)
	if err != nil {
		_ = j.Close()
		return bootJournal{}, err
	}
	torn, err := j.Open()
	if err != nil {
		_ = j.Close()
		return bootJournal{}, err
	}
	in.Renderer.Log(in.Log, torn...)
	if in.InMemoryJournal {
		s.kept = &memoryJournal{Journal: j, carried: carried}
		return s.kept.boot(), nil
	}
	return bootJournal{Journal: j, carried: carried, sessionID: j.SessionID, end: func(runErr error) error {
		if err := j.Close(); runErr == nil {
			return err
		}
		return nil
	}}, nil
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

func (j *memoryJournal) boot() bootJournal {
	return bootJournal{Journal: j, carried: j.carried, sessionID: j.SessionID, end: func(runErr error) error {
		if errors.Is(runErr, machine.ErrCrashed) {
			return j.snapshot()
		}
		return nil
	}}
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

func (j *memoryJournal) flush(log io.Writer, renderer render.Renderer) error {
	if !j.hasState {
		return nil
	}
	if err := j.Journal.WriteState(j.state); err != nil {
		var cause []int
		if j.state.LastSeq > 0 {
			cause = []int{j.state.LastSeq}
		}
		warning, err := j.Append(&journal.SessionWarning{Operation: "write state projection", Error: err.Error()}, cause...)
		if err != nil {
			return err
		}
		renderer.Log(log, warning)
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
