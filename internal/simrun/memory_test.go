package simrun

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
)

func TestInMemoryJournalMatchesFileBacked(t *testing.T) {
	t.Parallel()
	for _, machineFile := range []string{"", "../../tools/bench/machines/shared-rail.toml"} {
		name := "default"
		if machineFile != "" {
			name = "shared-rail"
		}
		for _, seed := range []uint64{1, 2, 3} {
			t.Run(fmt.Sprintf("%s/%d", name, seed), func(t *testing.T) {
				t.Parallel()
				cfg := sim.Config{Seed: seed}
				if machineFile != "" {
					var err error
					cfg, err = sim.LoadMachine(machineFile)
					if err != nil {
						t.Fatal(err)
					}
					cfg.Seed = seed
				}
				// Use the same path because backend argv in the journal names the state directory.
				dir := t.TempDir()
				var wantEvents, wantState []byte
				for _, inMemory := range []bool{false, true} {
					m, err := sim.New(cfg)
					if err != nil {
						t.Fatal(err)
					}
					stop, err := Simulate(context.Background(), Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Rotations: 1, InMemoryJournal: inMemory})
					if err != nil {
						t.Fatal(err)
					}
					if stop.Reason != session.StopRotations {
						t.Fatalf("in-memory %t: stopped with %+v", inMemory, stop)
					}
					events, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
					if err != nil {
						t.Fatal(err)
					}
					state, err := os.ReadFile(filepath.Join(dir, "state.json"))
					if err != nil {
						t.Fatal(err)
					}
					if inMemory {
						if diff := cmp.Diff(string(wantEvents), string(events)); diff != "" {
							t.Errorf("events.jsonl (-file-backed +in-memory):\n%s", diff)
						}
						if diff := cmp.Diff(string(wantState), string(state)); diff != "" {
							t.Errorf("state.json (-file-backed +in-memory):\n%s", diff)
						}
					} else {
						wantEvents, wantState = events, state
						if err := os.RemoveAll(dir); err != nil {
							t.Fatal(err)
						}
					}
				}
			})
		}
	}
}

func TestInMemoryProjectionFailureWarnsAfterStop(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	if err := os.Mkdir(filepath.Join(dir, "state.json"), 0755); err != nil {
		t.Fatal(err)
	}
	m, err := sim.New(sim.Config{Seed: 1})
	if err != nil {
		t.Fatal(err)
	}
	stop, err := Simulate(context.Background(), Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Rotations: 1, InMemoryJournal: true})
	if err != nil {
		t.Fatalf("projection failure must not fail a concluded session: %v", err)
	}
	if stop.Reason != session.StopRotations {
		t.Fatalf("stopped with %+v, want requested rotations", stop)
	}
	events, _, err := journal.Read(dir)
	if err != nil {
		t.Fatal(err)
	}
	last := events[len(events)-1]
	warning, ok := last.Data.(*journal.SessionWarning)
	if !ok || warning.Operation != "write state projection" || warning.Error == "" {
		t.Fatalf("last event = %+v, want projection failure warning", last)
	}
	if events[len(events)-2].Kind != journal.KindShutdown {
		t.Fatalf("event before warning = %s, want shutdown", events[len(events)-2].Kind)
	}
	if diff := cmp.Diff([]int{events[len(events)-2].Seq}, last.Cause); diff != "" {
		t.Fatalf("warning cause (-want +got):\n%s", diff)
	}
}
