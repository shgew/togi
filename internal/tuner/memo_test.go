package tuner_test

import (
	"context"
	"errors"
	"fmt"
	"path/filepath"
	"testing"

	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/simrun"
	"github.com/shgew/togi/internal/tuner"
)

// TestMemosMatchRecomputation replays simulated sessions event by event, asking for the next action wherever the
// runner would, and requires every index and memo the tuner still holds as valid to equal a fresh computation.
func TestMemosMatchRecomputation(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		machine string
		seed    uint64
		boots   int
	}{
		{"flat-hazard.toml", 7, 30},
		{"target-shared-voltage.toml", 2, 40},
		{"target-r7-request-gap.toml", 1, 40},
		{"misleading-mce.toml", 3, 40},
	} {
		t.Run(fmt.Sprintf("%s/%d", tc.machine, tc.seed), func(t *testing.T) {
			t.Parallel()
			cfg, err := sim.LoadMachine(filepath.Join("..", "..", "tools", "bench", "machines", tc.machine))
			if err != nil {
				t.Fatal(err)
			}
			cfg.Seed = tc.seed
			m, err := sim.New(cfg)
			if err != nil {
				t.Fatal(err)
			}
			dir := t.TempDir()
			_, err = simrun.Simulate(context.Background(), simrun.Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Cycles: 1, InMemoryJournal: true, MaxBoots: tc.boots})
			if err != nil && !errors.Is(err, simrun.ErrBootCap) {
				t.Fatal(err)
			}
			events, torn, err := journal.Read(dir)
			if err != nil || torn != nil {
				t.Fatalf("read simulation: %v, torn %q", err, torn)
			}
			s := tuner.New()
			running := false
			for i, e := range events {
				s.Fold(e)
				running = running || e.Kind == journal.KindTrialIntent
				if err := tuner.CheckMemos(s, i%64 == 0 || i == len(events)-1); err != nil {
					t.Fatalf("after event %d %s: %v", e.Seq, e.Kind, err)
				}
				if running && i+1 < len(events) && asked(events[i+1].Data) {
					s.Next()
				}
			}
			t.Logf("%d events", len(events))
		})
	}
}

// asked reports whether the runner asks the tuner for its next action just before appending p.
func asked(p journal.Payload) bool {
	switch p.(type) {
	case *journal.TrialIntent, *journal.HostRanking, *journal.TunerDecision, *journal.CorePhase, *journal.CheckingCycle, *journal.CheckingStep, *journal.CheckingChain, *journal.ProfileChange, *journal.HuntStart, *journal.HuntGroup, *journal.HuntEnd, *journal.HuntSkipped, *journal.Combination, *journal.DeepeningRound:
		return true
	}
	return false
}
