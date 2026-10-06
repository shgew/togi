package simrun

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
)

func sharedVoltageConfig(t *testing.T, seed uint64) sim.Config {
	t.Helper()
	cfg, err := sim.LoadMachine("../../tools/bench/machines/shared-voltage.toml")
	if err != nil {
		t.Fatal(err)
	}
	cfg.Seed = seed
	return cfg
}

func TestSixteenCoresReachCleanCycle(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	m, err := sim.New(sharedVoltageConfig(t, 1000))
	if err != nil {
		t.Fatal(err)
	}
	stop, err := Simulate(context.Background(), Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Cycles: 1})
	if err != nil {
		t.Fatal(err)
	}
	if stop.Reason != session.StopCycles {
		t.Fatalf("stopped with %+v", stop)
	}
	st, err := journal.ReadState(dir)
	if err != nil {
		t.Fatal(err)
	}
	if len(st.Cores) != 16 || st.Deepening != nil || st.Checking == nil || st.Checking.CleanCycles == 0 {
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
	passedFullCycle := false
	parts := map[int]map[int]int{}
	var combinations []journal.Combination
	failed := map[int]int{}
	multiR7 := map[string]bool{}
	named := map[int]int{}
	measuredReorder, namedMultiCount := false, false
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.CheckingCycle:
			if p.Event == journal.CycleEnd && p.Passed && p.Full {
				passedFullCycle = true
			}
		case *journal.TrialIntent:
			multiR7[p.Trial] = p.Regime == machine.R7 && len(p.Cores) > 1
			if p.Regime == machine.R7 && p.Phase == journal.PhaseChecking {
				if parts[p.Cycle] == nil {
					parts[p.Cycle] = map[int]int{}
				}
				parts[p.Cycle][p.DurationS]++
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
			// Multi-core R7 failures become failure points only through the core each voltage-targeted backoff moves.
			if !multiR7[p.Trial] && p.Attribution == journal.Attributed && p.Core != nil && p.Offset != nil {
				failed[*p.Core] = *p.Offset
			}
			if multiR7[p.Trial] && p.Attribution == journal.Attributed && p.Core != nil {
				named[e.Seq] = *p.Core
			}
		case *journal.TunerDecision:
			if p.FailurePoint != nil {
				failed[p.Core] = *p.FailurePoint
			}
			if len(e.Cause) == 0 {
				break
			}
			if core, ok := named[e.Cause[0]]; ok && p.Decision == journal.Backoff {
				if p.Core != core {
					t.Errorf("named R7 failure #%d on core %d backed off core %d", e.Cause[0], core, p.Core)
				}
				namedMultiCount = namedMultiCount || p.ToOffset-p.FromOffset > 1 && strings.Contains(p.Reason, "voltage-targeted R7 backoff")
			}
		case *journal.CheckingChain:
			// A measured order that differs from offset order shows requests, not offsets, ranked the cores.
			var loaded []int
			for _, group := range p.Groups {
				loaded = append(loaded, group...)
			}
			shallowest := -50
			for _, core := range loaded {
				shallowest = max(shallowest, p.Profile[core])
			}
			if len(p.SourceSeqs) > 0 && len(p.Groups) > 0 && slices.ContainsFunc(p.Groups[0], func(core int) bool { return p.Profile[core] != shallowest }) {
				measuredReorder = true
			}
		case *journal.HuntStart:
			if p.Regime == machine.R7 && len(p.Cores) > 1 {
				t.Errorf("multi-core R7 failure started hunt %d", p.Hunt)
			}
		case *journal.Combination:
			combinations = append(combinations, *p)
		}
	}
	if !measuredReorder {
		t.Error("no checking chain ordered its partial by measured requests that differ from offset order")
	}
	if !namedMultiCount {
		t.Error("no named R7 computation error moved its core by a multi-count voltage-targeted backoff")
	}
	if !passedFullCycle {
		t.Error("no passed full cycle end")
	}
	found := false
	for _, durations := range parts {
		if durations[120] >= 9 && durations[300] >= 6 && durations[600] >= 3 {
			found = true
			break
		}
	}
	if !found {
		t.Errorf("R7 parts did not run three trials of 120s and long 300/300/600s: %v", parts)
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
	_, err = Simulate(context.Background(), Input{Config: config.Default(), Dir: dir, Machine: m, Cycles: 1})
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

func TestBootCapLeavesPartialSessionInJournal(t *testing.T) {
	t.Parallel()
	m, err := sim.New(huntConfig(2))
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	_, err = Simulate(context.Background(), Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Cycles: 1, InMemoryJournal: true, MaxBoots: 3})
	if !errors.Is(err, ErrBootCap) {
		t.Fatalf("capped run: %v, want ErrBootCap", err)
	}
	events, torn, err := journal.Read(dir)
	if err != nil || torn != nil {
		t.Fatalf("read capped journal: %v, torn %q", err, torn)
	}
	boots := make(map[string]bool)
	crashes := 0
	for _, e := range events {
		boots[e.Boot] = true
		if e.Kind == journal.KindCrashDetected {
			crashes++
		}
	}
	if diff := cmp.Diff([]int{3, 2}, []int{len(boots), crashes}); diff != "" {
		t.Fatalf("boots and detected crashes (-want +got):\n%s", diff)
	}
	st, err := journal.ReadState(dir)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(events[len(events)-1].Seq, st.LastSeq); diff != "" {
		t.Fatalf("state after the cap (-want +got):\n%s", diff)
	}
}

func TestBootCapWithFailedFlushIsNotErrBootCap(t *testing.T) {
	t.Parallel()
	m, err := sim.New(huntConfig(2))
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	boots := 0
	wrap := func(j session.Journal) session.Journal {
		boots++
		if boots < 3 {
			return j
		}
		// On the last boot, a directory at state.json fails the final state write and a projection citing no earlier
		// event fails its session.warning.
		statePath := filepath.Join(dir, "state.json")
		if err := os.RemoveAll(statePath); err != nil {
			t.Fatal(err)
		}
		if err := os.Mkdir(statePath, 0o755); err != nil {
			t.Fatal(err)
		}
		return unflushableState{j}
	}
	_, err = Simulate(context.Background(), Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Cycles: 1, InMemoryJournal: true, MaxBoots: 3, Wrap: wrap})
	if errors.Is(err, ErrBootCap) {
		t.Fatalf("capped run with failed flush: %v, want an error that is not ErrBootCap", err)
	}
	if err == nil || !strings.Contains(err.Error(), "boot cap") || !strings.Contains(err.Error(), "not an earlier event") {
		t.Fatalf("capped run with failed flush: %v, want the cap and the failed session.warning", err)
	}
}

type unflushableState struct {
	session.Journal
}

func (j unflushableState) WriteState(s journal.State) error {
	s.LastSeq = 1 << 30
	return j.Journal.WriteState(s)
}

func TestNewSessionTrialGetsNoSamplesFromAnEarlierInvocation(t *testing.T) {
	t.Parallel()
	m, err := sim.New(huntConfig(4))
	if err != nil {
		t.Fatal(err)
	}
	in := Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: t.TempDir(), Machine: m, Cycles: 1}
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
