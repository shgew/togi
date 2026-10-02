package trialfacts

import (
	"fmt"
	"path/filepath"

	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/sim"
)

func LoadReplay(machinePath string, cfg sim.Config) (*sim.Replay, error) {
	if cfg.Facts == "" {
		return nil, fmt.Errorf("load replay oracle: machine has no facts extract")
	}
	path := cfg.Facts
	if !filepath.IsAbs(path) {
		path = filepath.Join(filepath.Dir(machinePath), path)
	}
	records, err := Read(path)
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
