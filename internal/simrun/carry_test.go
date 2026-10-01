package simrun

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
)

var (
	ruleset3Once  sync.Once
	ruleset3Files map[string][]byte
	ruleset3Err   error
)

func ruleset3Session(t *testing.T) (dir, id string) {
	t.Helper()
	ruleset3Once.Do(func() {
		src := t.TempDir()
		m, err := sim.New(sim.Config{Seed: 1})
		if err != nil {
			ruleset3Err = err
			return
		}
		if _, err := Simulate(context.Background(), Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: src, Machine: m, Rotations: 1}); err != nil {
			ruleset3Err = err
			return
		}
		entries, err := os.ReadDir(src)
		if err != nil {
			ruleset3Err = err
			return
		}
		ruleset3Files = make(map[string][]byte, len(entries))
		for _, e := range entries {
			data, err := os.ReadFile(filepath.Join(src, e.Name()))
			if err != nil {
				ruleset3Err = err
				return
			}
			ruleset3Files[e.Name()] = data
		}
	})
	if ruleset3Err != nil {
		t.Fatal(ruleset3Err)
	}
	dir = t.TempDir()
	for name, data := range ruleset3Files {
		if err := os.WriteFile(filepath.Join(dir, name), data, 0o644); err != nil {
			t.Fatal(err)
		}
	}
	return dir, stampRuleset3(t, dir)
}

func stampRuleset3(t *testing.T, dir string) string {
	t.Helper()
	path := filepath.Join(dir, "events.jsonl")
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	first, rest, _ := bytes.Cut(data, []byte{'\n'})
	current := session.Build().Ruleset
	restamped := bytes.Replace(first, fmt.Appendf(nil, `"ruleset":%d,`, current), []byte(`"ruleset":3,`), 1)
	if bytes.Equal(restamped, first) {
		t.Fatalf("session.start carries no ruleset %d stamp: %s", current, first)
	}
	if err := os.WriteFile(path, append(append(restamped, '\n'), rest...), 0o644); err != nil {
		t.Fatal(err)
	}
	stamp, id, err := journal.Scan(dir)
	if err != nil || stamp.Ruleset != 3 {
		t.Fatalf("scan: %+v, %v", stamp, err)
	}
	return id
}

func simulateAgain(t *testing.T, dir string, cfg sim.Config, c config.Config) (session.Stop, []journal.Event) {
	t.Helper()
	resumed, err := sim.Resume(dir, cfg)
	if err != nil {
		t.Fatal(err)
	}
	m, err := sim.New(resumed)
	if err != nil {
		t.Fatal(err)
	}
	stop, err := Simulate(context.Background(), Input{Config: c, ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Rotations: 1})
	if err != nil {
		t.Fatal(err)
	}
	events, _, err := journal.Read(dir)
	if err != nil {
		t.Fatal(err)
	}
	return stop, events
}

func carriedEvent(t *testing.T, events []journal.Event) *journal.SessionCarried {
	t.Helper()
	var carried []*journal.SessionCarried
	for _, e := range events {
		if p, ok := e.Data.(*journal.SessionCarried); ok {
			carried = append(carried, p)
		}
	}
	if len(carried) != 1 {
		t.Fatalf("%d session.carried events, want 1", len(carried))
	}
	return carried[0]
}

func firstPhases(events []journal.Event) map[int]*journal.CorePhase {
	phases := map[int]*journal.CorePhase{}
	for _, e := range events {
		if p, ok := e.Data.(*journal.CorePhase); ok && phases[p.Core] == nil {
			phases[p.Core] = p
		}
	}
	return phases
}

func TestARulesetTransitionSeedsTheNextSession(t *testing.T) {
	t.Parallel()
	dir, id := ruleset3Session(t)
	stop, events := simulateAgain(t, dir, sim.Config{Seed: 1}, config.Default())
	if stop.Reason != session.StopRotations {
		t.Fatalf("stopped with %+v", stop)
	}
	if _, err := os.Stat(filepath.Join(dir, "archive", id+".jsonl")); err != nil {
		t.Fatalf("archived session: %v", err)
	}
	carried := carriedEvent(t, events)
	if carried.Sources[0].Session != id || !carried.Marks {
		t.Fatalf("session.carried %+v, want marks from %s", carried, id)
	}
	phases := firstPhases(events)
	marks := map[int]int{}
	var edges, withMarks int
	for _, cc := range carried.Carried {
		p := phases[cc.Core]
		if cc.FailedMark != nil {
			withMarks++
			marks[cc.Core] = *cc.FailedMark
			if p.FailedMark == nil || *p.FailedMark != *cc.FailedMark || p.Offset <= *cc.FailedMark {
				t.Errorf("core %02d starts %+v, carried mark %d", cc.Core, p, *cc.FailedMark)
			}
		}
		if cc.Edge != nil {
			edges++
			want := *cc.Edge
			if cc.FailedMark != nil {
				want = max(want, *cc.FailedMark+1)
			}
			if p.To != journal.PhaseSearch || !p.CheckEdge || p.Offset != want {
				t.Errorf("core %02d starts %s at %d (check_edge %v), want checking search at %d", cc.Core, p.To, p.Offset, p.CheckEdge, want)
			}
		}
	}
	if edges == 0 || withMarks == 0 {
		t.Fatalf("carried %d edges and %d marks, want some of each", edges, withMarks)
	}
	for _, e := range events {
		if p, ok := e.Data.(*journal.TrialIntent); ok && p.Condition == machine.Isolated && p.Core != nil && p.Offset != nil {
			if m, ok := marks[*p.Core]; ok && *p.Offset <= m {
				t.Errorf("seq %d tests core %02d at %d, at or past its carried mark %d", e.Seq, *p.Core, *p.Offset, m)
			}
		}
	}
}

func TestARulesetTransitionAfterABIOSChangeCarriesOnlyEdges(t *testing.T) {
	t.Parallel()
	dir, _ := ruleset3Session(t)
	bios := machine.BIOSContext{BIOSVersion: "changed", Board: "board", CPUModel: "cpu", Microcode: "0x1", BoostLimitMHz: 5000}
	_, events := simulateAgain(t, dir, sim.Config{Seed: 1, BIOSContext: bios}, config.Default())
	carried := carriedEvent(t, events)
	if carried.Marks || !strings.Contains(carried.Detail, "bios_version") {
		t.Fatalf("session.carried %+v, want marks left behind for the BIOS change", carried)
	}
	if len(carried.Carried) == 0 {
		t.Fatal("no edges carried across the BIOS change")
	}
	for core, p := range firstPhases(events) {
		if p.FailedMark != nil {
			t.Errorf("core %02d starts with failed mark %d", core, *p.FailedMark)
		}
	}
}

func TestAConfiguredCandidateEdgeStopsShortOfACarriedMark(t *testing.T) {
	t.Parallel()
	dir, _ := ruleset3Session(t)
	c := config.Default()
	c.CandidateEdges = map[int]int{0: -50}
	_, events := simulateAgain(t, dir, sim.Config{Seed: 1}, c)
	var mark *int
	for _, cc := range carriedEvent(t, events).Carried {
		if cc.Core == 0 {
			mark = cc.FailedMark
		}
	}
	if mark == nil || *mark <= -50 {
		t.Fatalf("core 00 carried mark %v, want one shallower than -50", mark)
	}
	p := firstPhases(events)[0]
	if p.To != journal.PhaseSearch || !p.CheckEdge || p.Offset != *mark+1 || !strings.HasPrefix(p.Reason, fmt.Sprintf("configured candidate edge; clamped to %d", *mark+1)) {
		t.Fatalf("core 00 starts %+v, want checking search at %d", p, *mark+1)
	}
}

func TestACarriedMarkAtZeroDeadEndsTheCore(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	probe, err := sim.New(sim.Config{Seed: 1})
	if err != nil {
		t.Fatal(err)
	}
	bios, err := probe.Seams().Host.BIOSContext()
	if err != nil {
		t.Fatal(err)
	}
	now := time.Date(2026, 9, 1, 0, 0, 0, 0, time.UTC)
	j, err := journal.Open(dir, journal.Options{Boot: "old", Now: func() time.Time { return now }})
	if err != nil {
		t.Fatal(err)
	}
	for _, p := range []journal.Payload{
		&journal.SessionStart{Schema: journal.Schema, Ruleset: 2, Session: "20260901T000000Z"},
		&journal.SessionContext{BIOSContext: bios},
		&journal.TrialIntent{Trial: "0001", Core: new(0), Offset: new(0), Regime: machine.R6, Condition: machine.Isolated},
		&journal.TrialEnd{Trial: "0001", Outcome: journal.OutcomeFailure, Signal: machine.UnexpectedExit, Core: new(0)},
		&journal.Failure{Signal: machine.UnexpectedExit, Attribution: journal.Attributed, Core: new(0), Offset: new(0), Trial: "0001", Regime: machine.R6, Condition: machine.Isolated},
	} {
		if _, err := j.Append(p); err != nil {
			t.Fatal(err)
		}
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	stop, _ := simulateAgain(t, dir, sim.Config{Seed: 1}, config.Default())
	if stop.Reason != session.StopDeadEnd || stop.DeadEnd.Condition != journal.DeadEndFailureAtZero || stop.DeadEnd.Core == nil || *stop.DeadEnd.Core != 0 {
		t.Fatalf("stopped with %+v, want core 00 dead-ended at CO 0", stop)
	}
}

func TestBIOSArchiveInterruptedBeforeMoveResumesCarry(t *testing.T) {
	t.Parallel()
	cfg := huntConfig(4)
	m, err := sim.New(cfg)
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	c := quickMatrixConfig()
	c.CandidateEdges = map[int]int{0: -10, 1: -10, 2: -10, 3: -10}
	in := Input{Config: c, ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Rotations: 1}
	stopAfterIsolatedPasses(&in, 4)
	if _, err := Simulate(context.Background(), in); err != nil {
		t.Fatal(err)
	}
	_, id, err := journal.Scan(dir)
	if err != nil {
		t.Fatal(err)
	}
	m.SetBIOSContext(machine.BIOSContext{BIOSVersion: "changed", Board: "sim", CPUModel: "sim", Microcode: "0x2", BoostLimitMHz: 5500})
	m.Reboot()
	j, err := journal.OpenForArchive(dir, journal.Options{Boot: "interrupted", Now: m.Now})
	if err != nil {
		t.Fatal(err)
	}
	rel, archiveErr := j.ArchiveForCarry(id)
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	if archiveErr != nil {
		t.Fatal(archiveErr)
	}
	if err := os.Rename(filepath.Join(dir, rel), filepath.Join(dir, "events.jsonl")); err != nil {
		t.Fatal(err)
	}
	in.Until = func(e journal.Event) bool { return e.Kind == journal.KindSessionCarried }
	if _, err := Simulate(context.Background(), in); err != nil {
		t.Fatal(err)
	}
	events, torn, err := journal.Read(dir)
	if err != nil || torn != nil {
		t.Fatalf("read new session: %v, torn %q", err, torn)
	}
	carried := carriedEvent(t, events)
	if carried.Marks || len(carried.Carried) == 0 || carried.Sources[0].Session != id {
		t.Fatalf("interrupted BIOS archive carry %+v", carried)
	}
	if _, err := os.Stat(filepath.Join(dir, rel)); err != nil {
		t.Fatalf("interrupted archive was not finished: %v", err)
	}
}

func TestRulesetTransitionCarriesCulpritAndDirectHuntMarks(t *testing.T) {
	t.Parallel()
	for _, result := range []string{"culprit", "direct"} {
		t.Run(result, func(t *testing.T) {
			t.Parallel()
			cfg := huntConfig(4)
			c := config.Default()
			if result == "culprit" {
				for i := range cfg.Edges {
					cfg.Edges[i].Isolated = [5]int{-50, -50, -50, -50, -50}
					cfg.Edges[i].Resident = [7]int{-50, -50, -50, -50, -50, -50, -50}
				}
				cfg.Joints = []sim.Joint{{Members: map[int]int{1: -20}, Regimes: []machine.Regime{machine.R7}, Rate: 10}}
				c.CandidateEdges = map[int]int{0: -10, 1: -10, 2: -10, 3: -10}
			} else {
				cfg.Edges[1].Resident[6] = -5
			}
			finished := false
			_, source, dir := runHunt(t, cfg, nil, func(in *Input) {
				in.Config = c
				in.Until = func(e journal.Event) bool {
					if p, ok := e.Data.(*journal.HuntEnd); ok && p.Result == result && len(p.Cores) == 1 && p.Cores[0] == 1 {
						finished = true
						return result == "direct"
					}
					if p, ok := e.Data.(*journal.TunerDecision); ok && finished && result == "culprit" && p.Phase == journal.PhaseHunt && p.Decision == journal.Backoff && p.Core == 1 {
						return true
					}
					return false
				}
			})
			end, ok := findPayload(source, func(p *journal.HuntEnd) bool {
				return p.Result == result && len(p.Cores) == 1 && p.Cores[0] == 1
			})
			if !ok {
				t.Fatalf("no %s hunt on core 1", result)
			}
			start, ok := findPayload(source, func(p *journal.HuntStart) bool { return p.Hunt == end.Hunt })
			if !ok {
				t.Fatal("missing hunt start")
			}
			wantOffset := start.Failing[1]
			markSeq := 0
			signal := machine.Crash
			if result == "culprit" {
				for _, e := range source {
					if p, ok := e.Data.(*journal.HuntEnd); ok && p.Hunt == end.Hunt {
						markSeq = e.Seq
					}
				}
			} else {
				for _, e := range source {
					if p, ok := e.Data.(*journal.Failure); ok && p.Core != nil && *p.Core == 1 && p.Offset != nil && *p.Offset == wantOffset {
						markSeq, signal = e.Seq, p.Signal
					}
				}
			}
			if markSeq == 0 {
				t.Fatalf("%s hunt has no mark source", result)
			}
			id := stampRuleset3(t, dir)
			resumed, err := sim.Resume(dir, cfg)
			if err != nil {
				t.Fatal(err)
			}
			m, err := sim.New(resumed)
			if err != nil {
				t.Fatal(err)
			}
			if _, err := Simulate(context.Background(), Input{
				Config: c, ConfigPath: config.DefaultPath, Dir: dir, Machine: m,
				Until: func(e journal.Event) bool { return e.Kind == journal.KindSessionCarried },
			}); err != nil {
				t.Fatal(err)
			}
			events, _, err := journal.Read(dir)
			if err != nil {
				t.Fatal(err)
			}
			carried := carriedEvent(t, events)
			if !carried.Marks || carried.Sources[0].Session != id {
				t.Fatalf("%s carry %+v", result, carried)
			}
			var mark *journal.CarriedCore
			for i := range carried.Carried {
				if carried.Carried[i].Core == 1 {
					mark = &carried.Carried[i]
				}
			}
			if mark == nil || mark.FailedMark == nil || *mark.FailedMark != wantOffset || mark.MarkSeq != markSeq || mark.MarkSignal != signal {
				t.Fatalf("%s carried core 1 %+v, want offset %d from #%d (%s)", result, mark, wantOffset, markSeq, signal)
			}
		})
	}
}
