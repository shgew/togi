package trialfacts

import (
	"errors"
	"fmt"
	"path/filepath"

	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/sim"
)

// Extracts holds the records of each extract read so far, by resolved path.
type Extracts map[string][]Record

// Load returns the path and records of the machine's extract, which resolves
// relative to the machine file.
func (e Extracts) Load(machinePath string, cfg sim.Config) (string, []Record, error) {
	if cfg.Facts == "" {
		return "", nil, errors.New("machine has no facts extract")
	}
	path := cfg.Facts
	if !filepath.IsAbs(path) {
		path = filepath.Join(filepath.Dir(machinePath), path)
	}
	records, ok := e[path]
	if !ok {
		var err error
		records, err = Read(path)
		if err != nil {
			return "", nil, err
		}
		e[path] = records
	}
	return path, records, nil
}

func (e Extracts) Replay(machinePath string, cfg sim.Config) (*sim.Replay, error) {
	_, records, err := e.Load(machinePath, cfg)
	if err != nil {
		return nil, fmt.Errorf("load replay oracle: %w", err)
	}
	var trials []sim.ReplayFact
	for _, r := range records {
		if r.Kind != facts.TrialFact || r.Context == nil {
			continue
		}
		trials = append(trials, sim.ReplayFact{Context: *r.Context, Class: r.Class, Profile: r.Profile, Outcome: r.Outcome, Signal: r.Signal, DurationS: r.DurationS})
	}
	replay, err := sim.NewReplay(cfg.BIOSContext, trials)
	if err != nil {
		return nil, fmt.Errorf("load replay oracle: %w", err)
	}
	return replay, nil
}
