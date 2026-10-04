package simrun

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sync"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
)

func TestInMemoryJournalMatchesFileBacked(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name        string
		machineFile string
		cleanCycle  bool
	}{
		{name: "shared-voltage", machineFile: "../../tools/bench/machines/shared-voltage.toml", cleanCycle: true},
		{name: "legacy-default"},
		{name: "target-fit-0", machineFile: "../../tools/bench/machines/target-fit-0.toml"},
	} {
		seeds := []uint64{1, 2, 3}
		if tc.cleanCycle {
			seeds = []uint64{1000, 1001, 1002}
		}
		for _, seed := range seeds {
			t.Run(fmt.Sprintf("%s/%d", tc.name, seed), func(t *testing.T) {
				t.Parallel()
				cfg := sim.Config{Seed: seed}
				if tc.cleanCycle {
					cfg = sharedVoltageConfig(t, seed)
				} else if tc.machineFile != "" {
					var err error
					cfg, err = sim.LoadMachine(tc.machineFile)
					if err != nil {
						t.Fatal(err)
					}
					cfg.Seed = seed
				}
				// Backend argv in the journal names the state directory, so both runs replace it with one placeholder.
				var events, states [2][]byte
				var stops [2]session.Stop
				var wg sync.WaitGroup
				for i, inMemory := range []bool{false, true} {
					dir := t.TempDir()
					wg.Go(func() {
						m, err := sim.New(cfg)
						if err != nil {
							t.Error(err)
							return
						}
						stop, err := Simulate(context.Background(), Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Cycles: 1, InMemoryJournal: inMemory})
						if err != nil {
							t.Error(err)
							return
						}
						stops[i] = stop
						if tc.cleanCycle && stop.Reason != session.StopCycles {
							t.Errorf("in-memory %t: stopped with %+v", inMemory, stop)
							return
						}
						if events[i], err = os.ReadFile(filepath.Join(dir, "events.jsonl")); err != nil {
							t.Error(err)
						}
						if states[i], err = os.ReadFile(filepath.Join(dir, "state.json")); err != nil {
							t.Error(err)
						}
						for _, b := range []*[]byte{&events[i], &states[i]} {
							*b = bytes.ReplaceAll(*b, []byte(dir), []byte("STATE-DIR"))
						}
					})
				}
				wg.Wait()
				if t.Failed() {
					return
				}
				// Legacy independent-R7 models may dead-end; both journal modes must preserve the same outcome.
				if diff := cmp.Diff(stops[0], stops[1]); diff != "" {
					t.Errorf("stop (-file-backed +in-memory):\n%s", diff)
				}
				if diff := cmp.Diff(string(events[0]), string(events[1])); diff != "" {
					t.Errorf("events.jsonl (-file-backed +in-memory):\n%s", diff)
				}
				if diff := cmp.Diff(string(states[0]), string(states[1])); diff != "" {
					t.Errorf("state.json (-file-backed +in-memory):\n%s", diff)
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
	m, err := sim.New(sharedVoltageConfig(t, 1000))
	if err != nil {
		t.Fatal(err)
	}
	stop, err := Simulate(context.Background(), Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Cycles: 1, InMemoryJournal: true})
	if err != nil {
		t.Fatalf("projection failure must not fail a concluded session: %v", err)
	}
	if stop.Reason != session.StopCycles {
		t.Fatalf("stopped with %+v, want requested cycles", stop)
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

type crashBeforeProjection struct {
	session.Journal
	machine *sim.Machine
	fired   *bool
}

func (j crashBeforeProjection) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := j.Journal.Append(p, cause...)
	if err == nil && !*j.fired && p.Kind() == journal.KindSessionStart {
		*j.fired = true
		j.machine.Crash()
		return e, machine.ErrCrashed
	}
	return e, err
}

func TestInMemoryCrashBeforeFirstProjectionReplaysDurableStart(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	m, err := sim.New(huntConfig(2))
	if err != nil {
		t.Fatal(err)
	}
	fired := false
	stop, err := Simulate(context.Background(), Input{
		Config: config.Default(), Dir: dir, Machine: m, InMemoryJournal: true,
		Wrap: func(j session.Journal) session.Journal {
			return crashBeforeProjection{Journal: j, machine: m, fired: &fired}
		},
		Until: func(e journal.Event) bool { return e.Kind == journal.KindTrialEnd },
	})
	if err != nil || stop.Reason != session.StopSignal || !fired {
		t.Fatalf("bootstrap crash: %+v, %v, fired %t", stop, err, fired)
	}
	events, torn, err := journal.Read(dir)
	if err != nil || len(torn) != 0 {
		t.Fatalf("recovered journal: %v, torn %q", err, torn)
	}
	starts, crashes, trials := 0, 0, 0
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.SessionStart:
			starts++
		case *journal.CrashDetected:
			crashes++
			if !p.Stray || p.PreviousBoot != events[0].Boot || p.InFlight != nil {
				t.Fatalf("bootstrap crash misclassified: %+v", p)
			}
		case *journal.TrialEnd:
			trials++
		}
	}
	if diff := cmp.Diff([]int{1, 1, 1}, []int{starts, crashes, trials}); diff != "" {
		t.Fatalf("recovery duplicated work (-want +got):\n%s", diff)
	}
	if p, ok := events[len(events)-1].Data.(*journal.Shutdown); !ok || p.Reason != journal.ShutdownSignal {
		t.Fatalf("bootstrap recovery did not stop cleanly: %+v", events[len(events)-1])
	}
	state, err := journal.ReadState(dir)
	if err != nil {
		t.Fatal(err)
	}
	if state.LastSeq != events[len(events)-1].Seq || state.Session.ID != events[0].Data.(*journal.SessionStart).Session || len(state.Cores) != 2 {
		t.Fatalf("flushed state does not project recovered session: %+v", state)
	}
	if diff := cmp.Diff([]string(nil), m.Violations()); diff != "" {
		t.Fatalf("recovery broke isolation:\n%s", diff)
	}
}

func TestInMemoryResumeReadsMatchingDiskProjection(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	m, err := sim.New(huntConfig(2))
	if err != nil {
		t.Fatal(err)
	}
	in := Input{
		Config: config.Default(), Dir: dir, Machine: m, InMemoryJournal: true,
		Until: func(e journal.Event) bool { return e.Kind == journal.KindTrialEnd },
	}
	stop, err := Simulate(context.Background(), in)
	if err != nil || stop.Reason != session.StopSignal {
		t.Fatalf("initial simulation: %+v, %v", stop, err)
	}
	state, err := journal.ReadState(dir)
	if err != nil {
		t.Fatal(err)
	}
	stop, err = Simulate(context.Background(), in)
	if err != nil || stop.Reason != session.StopSignal {
		t.Fatalf("resumed simulation: %+v, %v", stop, err)
	}
	events, torn, err := journal.Read(dir)
	if err != nil || len(torn) != 0 {
		t.Fatalf("resumed journal: %v, torn %q", err, torn)
	}
	trials := 0
	for _, e := range events {
		if e.Seq <= state.LastSeq {
			continue
		}
		if e.Kind == journal.KindStateRebuilt {
			t.Fatalf("matching disk projection was spuriously rebuilt: %+v", e)
		}
		if e.Kind == journal.KindTrialEnd {
			trials++
		}
	}
	if diff := cmp.Diff(1, trials); diff != "" {
		t.Fatalf("resumed trials (-want +got):\n%s", diff)
	}
}

func TestInMemoryInvalidConfigLeavesNoProjection(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	m, err := sim.New(huntConfig(2))
	if err != nil {
		t.Fatal(err)
	}
	cfg := config.Default()
	cfg.StartOffsets = map[int]int{99: -10}
	_, err = Simulate(context.Background(), Input{Config: cfg, Dir: dir, Machine: m, InMemoryJournal: true})
	if !errors.Is(err, session.ErrNoSuchCore) {
		t.Fatalf("invalid cached simulation: %v", err)
	}
	if _, err := os.Stat(filepath.Join(dir, "state.json")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("rejected simulation published a projection: %v", err)
	}
	events, torn, err := journal.Read(dir)
	if err != nil || len(events) != 0 || len(torn) != 0 {
		t.Fatalf("rejected simulation began a session: %+v, torn %q, %v", events, torn, err)
	}
}
