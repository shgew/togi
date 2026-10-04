package simrun

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
)

func TestSixteenCoresReachCleanLap(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	m, err := sim.New(sim.Config{Seed: 1})
	if err != nil {
		t.Fatal(err)
	}
	stop, err := Simulate(context.Background(), Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Laps: 1})
	if err != nil {
		t.Fatal(err)
	}
	if stop.Reason != session.StopLaps {
		t.Fatalf("stopped with %+v", stop)
	}
	st, err := journal.ReadState(dir)
	if err != nil {
		t.Fatal(err)
	}
	if len(st.Cores) != 16 || st.Deepening != nil || st.Checking == nil || st.Checking.CleanLaps == 0 {
		t.Fatalf("state has %d cores, deepening %+v, checking %+v", len(st.Cores), st.Deepening, st.Checking)
	}
	for _, c := range st.Cores {
		if c.Phase != journal.PhaseAtLimit {
			t.Errorf("core %d is %s, want at its limit", c.Core, c.Phase)
		}
	}
	if diff := cmp.Diff([]string(nil), m.Violations()); diff != "" {
		t.Errorf("isolation violations (-want +got):\n%s", diff)
	}
	events, torn, err := journal.Read(dir)
	if err != nil || torn != nil {
		t.Fatalf("read journal: %v, torn %q", err, torn)
	}
	passedFullLap := false
	parts := map[int]map[int]int{}
	var combinations []journal.Combination
	failed := map[int]int{}
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.CheckingLap:
			if p.Event == journal.LapEnd && p.Passed && p.Full {
				passedFullLap = true
			}
		case *journal.TrialIntent:
			if p.Regime == machine.R7 && p.Phase == journal.PhaseChecking {
				if parts[p.Lap] == nil {
					parts[p.Lap] = map[int]int{}
				}
				parts[p.Lap][p.DurationS]++
			}
			for core, at := range failed {
				if p.Profile[core] <= at {
					t.Errorf("trial %s reaches failure point of core %d at %d", p.Trial, core, at)
				}
			}
			for _, combination := range combinations {
				reaches := true
				for _, member := range combination.Members {
					if p.Profile[member.Core] > member.Offset {
						reaches = false
						break
					}
				}
				if reaches {
					t.Errorf("trial %s reaches combination C%d", p.Trial, combination.Combination)
				}
			}
		case *journal.Failure:
			if p.Attribution == journal.Attributed && p.Core != nil && p.Offset != nil {
				failed[*p.Core] = *p.Offset
			}
		case *journal.Combination:
			combinations = append(combinations, *p)
		}
	}
	if !passedFullLap {
		t.Error("no passed full lap end")
	}
	found := false
	for _, durations := range parts {
		if durations[120] >= 9 && durations[300] >= 6 && durations[600] >= 3 {
			found = true
			break
		}
	}
	if !found {
		t.Errorf("R7 parts did not run three starts of 120s and long 300/300/600s: %v", parts)
	}
}

func TestSimulatorRefusesAnotherJournalWriter(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	m, err := sim.New(huntConfig(2))
	if err != nil {
		t.Fatal(err)
	}
	j, err := journal.Lock(dir, journal.Options{Now: m.Now, Build: session.Build()})
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	_, err = Simulate(context.Background(), Input{Config: config.Default(), Dir: dir, Machine: m, Laps: 1})
	if !errors.Is(err, journal.ErrLocked) {
		t.Fatalf("second writer: %v, want locked journal", err)
	}
	if _, err := os.Stat(filepath.Join(dir, "events.jsonl")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("refused simulator created a journal: %v", err)
	}
	for core := range 2 {
		if offset, err := m.Seams().SMU.Offset(core); err != nil || offset != 0 {
			t.Fatalf("refused simulator changed core %d: %d, %v", core, offset, err)
		}
	}
}

func TestNewSessionTrialGetsNoSamplesFromAnEarlierInvocation(t *testing.T) {
	t.Parallel()
	m, err := sim.New(huntConfig(4))
	if err != nil {
		t.Fatal(err)
	}
	in := Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: t.TempDir(), Machine: m, Laps: 1}
	in.Until = func(e journal.Event) bool {
		p, ok := e.Data.(*journal.TrialEnd)
		return ok && p.Trial == "0001"
	}
	if _, err := Simulate(context.Background(), in); err != nil {
		t.Fatal(err)
	}
	if len(slices.Collect(m.Seams().Trials.Samples("0001"))) == 0 {
		t.Fatal("first session's trial 0001 kept no samples to leak")
	}
	m.SetBIOSContext(machine.BIOSContext{BIOSVersion: "new", Board: "sim", CPUModel: "sim", Microcode: "0x2", BoostLimitMHz: 5500})
	m.Reboot()
	fired := false
	in.Wrap = func(j session.Journal) session.Journal {
		return &resetAtEvent{Journal: j, machine: m, kind: journal.KindTrialIntent, reset: machine.ResetWatchdog, fired: &fired}
	}
	if _, err := Simulate(context.Background(), in); err != nil {
		t.Fatal(err)
	}
	events, torn, err := journal.Read(in.Dir)
	if err != nil || torn != nil {
		t.Fatalf("read new session: %v, torn %q", err, torn)
	}
	end, ok := findPayload(events, func(p *journal.TrialEnd) bool { return p.Trial == "0001" })
	if !fired || !ok {
		t.Fatalf("crash at trial.intent fired %t, recovered trial.end found %t", fired, ok)
	}
	if end.LastSampleS != nil || end.StalledCore != nil || end.WorkerStalledMS != nil {
		t.Fatalf("trial 0001 never ran but recorded the earlier session's samples: %+v", end)
	}
}
