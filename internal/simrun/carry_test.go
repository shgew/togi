package simrun

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
)

// ruleset2Session runs a first session, then restamps its journal as ruleset 2 so the next run is a transition.
func ruleset2Session(t *testing.T) (dir, id string) {
	t.Helper()
	dir = t.TempDir()
	m, err := sim.New(sim.Config{Seed: 1})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := Simulate(context.Background(), Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Rotations: 1}); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(dir, "events.jsonl")
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	first, rest, _ := bytes.Cut(data, []byte{'\n'})
	restamped := bytes.Replace(first, []byte(`"ruleset":3,`), []byte(`"ruleset":2,`), 1)
	if bytes.Equal(restamped, first) {
		t.Fatalf("session.start carries no ruleset 3 stamp: %s", first)
	}
	if err := os.WriteFile(path, append(append(restamped, '\n'), rest...), 0o644); err != nil {
		t.Fatal(err)
	}
	stamp, id, err := journal.Scan(dir)
	if err != nil || stamp.Ruleset != 2 {
		t.Fatalf("scan: %+v, %v", stamp, err)
	}
	return dir, id
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
	dir, id := ruleset2Session(t)
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
			if p.To != journal.PhaseConfirmation || p.Offset != want {
				t.Errorf("core %02d starts %s at %d, want confirmation at %d", cc.Core, p.To, p.Offset, want)
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
	dir, _ := ruleset2Session(t)
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
	dir, _ := ruleset2Session(t)
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
	if p.To != journal.PhaseConfirmation || p.Offset != *mark+1 || !strings.HasPrefix(p.Reason, fmt.Sprintf("configured candidate edge; clamped to %d", *mark+1)) {
		t.Fatalf("core 00 starts %+v, want confirmation at %d", p, *mark+1)
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
