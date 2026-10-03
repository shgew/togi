package simrun

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/tuner"
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
		if _, err := Simulate(context.Background(), Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: src, Machine: m, Laps: 1}); err != nil {
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
			if e.IsDir() {
				continue
			}
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
	return stampRuleset(t, dir, 3)
}

func stampRuleset(t *testing.T, dir string, ruleset int) string {
	t.Helper()
	path := filepath.Join(dir, "events.jsonl")
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	first, rest, _ := bytes.Cut(data, []byte{'\n'})
	current := session.Build().Ruleset
	restamped := bytes.Replace(first, fmt.Appendf(nil, `"ruleset":%d,`, current), fmt.Appendf(nil, `"ruleset":%d,`, ruleset), 1)
	if bytes.Equal(restamped, first) {
		t.Fatalf("session.start carries no ruleset %d stamp: %s", current, first)
	}
	if err := os.WriteFile(path, append(append(restamped, '\n'), rest...), 0o644); err != nil {
		t.Fatal(err)
	}
	stamp, id, err := journal.Scan(dir)
	if err != nil || stamp.Ruleset != ruleset {
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
	stop, err := Simulate(context.Background(), Input{Config: c, ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Laps: 1})
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

func TestTransitionWithUnknownKinds(t *testing.T) {
	for _, tc := range []struct {
		name        string
		ruleset     int
		schema      int
		refuse      bool
		interrupted bool
	}{
		{"older ruleset", session.Build().Ruleset - 1, journal.Schema, false, false},
		{"interrupted older ruleset", session.Build().Ruleset - 1, journal.Schema, false, true},
		{"older schema", session.Build().Ruleset, journal.Schema - 1, false, false},
		{"current build", session.Build().Ruleset, journal.Schema, true, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			m, err := sim.New(sim.Config{Seed: 1})
			if err != nil {
				t.Fatal(err)
			}
			bios, err := m.Seams().Host.BIOSContext()
			if err != nil {
				t.Fatal(err)
			}
			const id = "20260901T000000Z"
			j, err := journal.Open(dir, journal.Options{Boot: "old"})
			if err != nil {
				t.Fatal(err)
			}
			for _, p := range []journal.Payload{
				&journal.SessionStart{Schema: journal.Schema, Ruleset: tc.ruleset, Session: id},
				&journal.SessionContext{BIOSContext: bios},
				&journal.TrialIntent{Trial: "0001", Core: new(0), Offset: new(-30), Regime: machine.R1, Condition: machine.Alone},
				&journal.TrialEnd{Trial: "0001", Outcome: journal.OutcomePass},
				&journal.TrialIntent{Trial: "0002", Core: new(0), Offset: new(-35), Regime: machine.R1, Condition: machine.Alone},
				&journal.TrialEnd{Trial: "0002", Outcome: journal.OutcomeFailure, Signal: machine.UnexpectedExit, Core: new(0)},
				&journal.Failure{Signal: machine.UnexpectedExit, Attribution: journal.Attributed, Core: new(0), Offset: new(-35), Trial: "0002", Regime: machine.R1, Condition: machine.Alone},
			} {
				if _, err := j.Append(p); err != nil {
					t.Fatal(err)
				}
			}
			if err := j.Close(); err != nil {
				t.Fatal(err)
			}
			path := filepath.Join(dir, "events.jsonl")
			original, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			original = bytes.Replace(original, fmt.Appendf(nil, `"schema":%d`, journal.Schema), fmt.Appendf(nil, `"schema":%d`, tc.schema), 1)
			lines := bytes.SplitAfter(original, []byte{'\n'})
			for n := 4; n < 7; n++ {
				lines[n] = bytes.Replace(lines[n], fmt.Appendf(nil, `"seq":%d`, n+1), fmt.Appendf(nil, `"seq":%d`, n+2), 1)
			}
			unknown := []byte("{\"seq\":5,\"time\":\"2026-09-01T00:00:00Z\",\"boot\":\"old\",\"kind\":\"retired.fact\",\"msg\":\"unknown evidence\",\"core\":0,\"offset\":-50}\n")
			original = bytes.Join([][]byte{bytes.Join(lines[:4], nil), unknown, bytes.Join(lines[4:], nil)}, nil)
			if err := os.WriteFile(path, original, 0o644); err != nil {
				t.Fatal(err)
			}
			if tc.interrupted {
				archiveDir := filepath.Join(dir, "archive")
				if err := os.MkdirAll(archiveDir, 0o755); err != nil {
					t.Fatal(err)
				}
				if err := os.WriteFile(filepath.Join(archiveDir, id+"-carry-pending"), nil, 0o644); err != nil {
					t.Fatal(err)
				}
			}
			_, err = Simulate(context.Background(), Input{
				Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m,
				Until: func(e journal.Event) bool { return e.Kind == journal.KindSessionCarried },
			})
			if tc.refuse {
				var unknown *journal.UnknownKindError
				if !errors.As(err, &unknown) || unknown.Kind != "retired.fact" {
					t.Fatalf("current journal refusal: %v", err)
				}
				after, readErr := os.ReadFile(path)
				if readErr != nil || !bytes.Equal(original, after) {
					t.Fatalf("refusal changed journal: %v", readErr)
				}
				if _, statErr := os.Stat(filepath.Join(dir, "archive", id+".jsonl")); !errors.Is(statErr, os.ErrNotExist) {
					t.Fatalf("refusal archived journal: %v", statErr)
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			archived, err := os.ReadFile(filepath.Join(dir, "archive", id+".jsonl"))
			if err != nil || !bytes.Equal(original, archived) {
				t.Fatalf("archive changed journal: %v", err)
			}
			events, _, err := journal.Read(dir)
			if err != nil {
				t.Fatal(err)
			}
			start := events[0].Data.(*journal.SessionStart)
			if start.Ruleset != session.Build().Ruleset || start.Schema != journal.Schema || start.Session == id {
				t.Fatalf("new session stamp: %+v", start)
			}
			carried := carriedEvent(t, events)
			if len(carried.Sources) != 1 || carried.Sources[0].Session != id || !carried.FailurePoints || len(carried.Carried) != 1 {
				t.Fatalf("carry: %+v", carried)
			}
			core := carried.Carried[0]
			if core.Core != 0 || core.SoloLimit == nil || *core.SoloLimit != -30 || core.SoloLimitSeq != 4 || core.FailurePoint == nil || *core.FailurePoint != -35 || core.FailurePointSeq != 8 {
				t.Fatalf("known-event carry: %+v", core)
			}
		})
	}
}

func TestARulesetTransitionSeedsTheNextSession(t *testing.T) {
	t.Parallel()
	dir, id := ruleset3Session(t)
	stop, events := simulateAgain(t, dir, sim.Config{Seed: 1}, config.Default())
	if stop.Reason != session.StopLaps {
		t.Fatalf("stopped with %+v", stop)
	}
	if _, err := os.Stat(filepath.Join(dir, "archive", id+".jsonl")); err != nil {
		t.Fatalf("archived session: %v", err)
	}
	carried := carriedEvent(t, events)
	if carried.Sources[0].Session != id || !carried.FailurePoints {
		t.Fatalf("session.carried %+v, want failure points from %s", carried, id)
	}
	phases := firstPhases(events)
	failurePoints := map[int]int{}
	var soloLimits, withFailurePoints int
	for _, cc := range carried.Carried {
		p := phases[cc.Core]
		if cc.FailurePoint != nil {
			withFailurePoints++
			failurePoints[cc.Core] = *cc.FailurePoint
			if p.FailurePoint == nil || *p.FailurePoint != *cc.FailurePoint || p.Offset <= *cc.FailurePoint {
				t.Errorf("core %02d starts %+v, carried failure point %d", cc.Core, p, *cc.FailurePoint)
			}
		}
		if cc.SoloLimit != nil {
			soloLimits++
			want := *cc.SoloLimit
			if cc.FailurePoint != nil {
				want = max(want, *cc.FailurePoint+1)
			}
			if p.To != journal.PhaseSearch || !p.CheckSoloLimit || p.Offset != want {
				t.Errorf("core %02d starts %s at %d (check_solo_limit %v), want checking search at %d", cc.Core, p.To, p.Offset, p.CheckSoloLimit, want)
			}
		}
	}
	if soloLimits == 0 || withFailurePoints == 0 {
		t.Fatalf("carried %d solo limits and %d failure points, want some of each", soloLimits, withFailurePoints)
	}
	for _, e := range events {
		if p, ok := e.Data.(*journal.TrialIntent); ok && p.Condition == machine.Alone && p.Core != nil && p.Offset != nil {
			if m, ok := failurePoints[*p.Core]; ok && *p.Offset <= m {
				t.Errorf("seq %d tests core %02d at %d, at or past its carried failure point %d", e.Seq, *p.Core, *p.Offset, m)
			}
		}
	}
}

func TestARulesetTransitionAfterABIOSChangeCarriesOnlySoloLimits(t *testing.T) {
	t.Parallel()
	dir, _ := ruleset3Session(t)
	bios := machine.BIOSContext{BIOSVersion: "changed", Board: "board", CPUModel: "cpu", Microcode: "0x1", BoostLimitMHz: 5000}
	_, events := simulateAgain(t, dir, sim.Config{Seed: 1, BIOSContext: bios}, config.Default())
	carried := carriedEvent(t, events)
	if carried.FailurePoints || !strings.Contains(carried.Detail, "bios_version") {
		t.Fatalf("session.carried %+v, want failure points left behind for the BIOS change", carried)
	}
	if len(carried.Carried) == 0 {
		t.Fatal("no solo limits carried across the BIOS change")
	}
	for core, p := range firstPhases(events) {
		if p.FailurePoint != nil {
			t.Errorf("core %02d starts with failure point %d", core, *p.FailurePoint)
		}
	}
}

func TestAConfiguredCandidateSoloLimitStopsShortOfACarriedFailurePoint(t *testing.T) {
	t.Parallel()
	dir, _ := ruleset3Session(t)
	c := config.Default()
	c.CandidateSoloLimits = map[int]int{0: -50}
	_, events := simulateAgain(t, dir, sim.Config{Seed: 1}, c)
	var failurePoint *int
	for _, cc := range carriedEvent(t, events).Carried {
		if cc.Core == 0 {
			failurePoint = cc.FailurePoint
		}
	}
	if failurePoint == nil || *failurePoint <= -50 {
		t.Fatalf("core 00 carried failure point %v, want one shallower than -50", failurePoint)
	}
	p := firstPhases(events)[0]
	if p.To != journal.PhaseSearch || !p.CheckSoloLimit || p.Offset != *failurePoint+1 || !strings.HasPrefix(p.Reason, fmt.Sprintf("configured candidate solo limit; clamped to %d", *failurePoint+1)) {
		t.Fatalf("core 00 starts %+v, want checking search at %d", p, *failurePoint+1)
	}
}

func TestACarriedFailurePointAtZeroDeadEndsTheCore(t *testing.T) {
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
		&journal.TrialIntent{Trial: "0001", Core: new(0), Offset: new(0), Regime: machine.R6, Condition: machine.Alone},
		&journal.TrialEnd{Trial: "0001", Outcome: journal.OutcomeFailure, Signal: machine.UnexpectedExit, Core: new(0)},
		&journal.Failure{Signal: machine.UnexpectedExit, Attribution: journal.Attributed, Core: new(0), Offset: new(0), Trial: "0001", Regime: machine.R6, Condition: machine.Alone},
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
	c.CandidateSoloLimits = map[int]int{0: -10, 1: -10, 2: -10, 3: -10}
	in := Input{Config: c, ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Laps: 1}
	stopAfterAlonePasses(&in, 4)
	if _, err := Simulate(context.Background(), in); err != nil {
		t.Fatal(err)
	}
	_, id, err := journal.Scan(dir)
	if err != nil {
		t.Fatal(err)
	}
	m.SetBIOSContext(machine.BIOSContext{BIOSVersion: "changed", Board: "sim", CPUModel: "sim", Microcode: "0x2", BoostLimitMHz: 5500})
	m.Reboot()
	j, err := journal.Lock(dir, journal.Options{Boot: "interrupted", Now: m.Now})
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
	if err := os.Rename(filepath.Join(dir, "archive", id+"-trials"), filepath.Join(dir, "trials")); err != nil {
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
	if carried.FailurePoints || len(carried.Carried) == 0 || carried.Sources[0].Session != id {
		t.Fatalf("interrupted BIOS archive carry %+v", carried)
	}
	if _, err := os.Stat(filepath.Join(dir, rel)); err != nil {
		t.Fatalf("interrupted archive was not finished: %v", err)
	}
}

func TestRulesetTransitionCarriesCulpritAndDirectHuntFailurePoints(t *testing.T) {
	t.Parallel()
	for _, result := range []string{"culprit", "direct"} {
		t.Run(result, func(t *testing.T) {
			t.Parallel()
			cfg := huntConfig(4)
			c := config.Default()
			if result == "culprit" {
				for i := range cfg.Limits {
					cfg.Limits[i].Alone = [5]int{-50, -50, -50, -50, -50}
					cfg.Limits[i].Together = [7]int{-50, -50, -50, -50, -50, -50, -50}
				}
				cfg.Combinations = []sim.Combination{{Members: map[int]int{1: -20}, Regimes: []machine.Regime{machine.R7}, Rate: 10}}
				c.CandidateSoloLimits = map[int]int{0: -10, 1: -10, 2: -10, 3: -10}
			} else {
				cfg.Limits[1].Together[6] = -5
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
			failurePointSeq := 0
			signal := machine.Crash
			if result == "culprit" {
				for _, e := range source {
					if p, ok := e.Data.(*journal.HuntEnd); ok && p.Hunt == end.Hunt {
						failurePointSeq = e.Seq
					}
				}
			} else {
				for _, e := range source {
					if p, ok := e.Data.(*journal.Failure); ok && p.Core != nil && *p.Core == 1 && p.Offset != nil && *p.Offset == wantOffset {
						failurePointSeq, signal = e.Seq, p.Signal
					}
				}
			}
			if failurePointSeq == 0 {
				t.Fatalf("%s hunt has no failure point source", result)
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
			if !carried.FailurePoints || carried.Sources[0].Session != id {
				t.Fatalf("%s carry %+v", result, carried)
			}
			var failurePoint *journal.CarriedCore
			for i := range carried.Carried {
				if carried.Carried[i].Core == 1 {
					failurePoint = &carried.Carried[i]
				}
			}
			if failurePoint == nil || failurePoint.FailurePoint == nil || *failurePoint.FailurePoint != wantOffset || failurePoint.FailurePointSeq != failurePointSeq || failurePoint.FailurePointSignal != signal {
				t.Fatalf("%s carried core 1 %+v, want offset %d from #%d (%s)", result, failurePoint, wantOffset, failurePointSeq, signal)
			}
		})
	}
}

func TestCurrentRulesetChecksCarriedSoloLimitsButCompletesFullLapsWithLivePasses(t *testing.T) {
	t.Parallel()
	cfg := huntConfig(2)
	c := config.Default()
	c.CandidateSoloLimits = map[int]int{0: -10, 1: -10}
	firstPassedFullLap := func(e journal.Event) bool {
		p, ok := e.Data.(*journal.CheckingLap)
		return ok && p.Event == journal.LapEnd && p.Passed && p.Full
	}
	_, source, dir := runHunt(t, cfg, nil, func(in *Input) {
		in.Config = c
		in.Until = firstPassedFullLap
	})
	id := stampRuleset(t, dir, 6)
	resumed, err := sim.Resume(dir, cfg)
	if err != nil {
		t.Fatal(err)
	}
	m, err := sim.New(resumed)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := Simulate(context.Background(), Input{
		Config: c, ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Until: firstPassedFullLap,
	}); err != nil {
		t.Fatal(err)
	}
	events, torn, err := journal.Read(dir)
	if err != nil || torn != nil {
		t.Fatalf("read transitioned session: %v, torn %q", err, torn)
	}
	if start := events[0].Data.(*journal.SessionStart); start.Ruleset != tuner.Ruleset {
		t.Fatalf("transition ruleset %d, want %d", start.Ruleset, tuner.Ruleset)
	}
	archived, err := facts.ReadJournal(filepath.Join(dir, "archive", id+".jsonl"))
	if err != nil || archived.Ruleset != 6 {
		t.Fatalf("archived source: %+v, %v", archived, err)
	}
	carried := map[int]*journal.TrialCarried{}
	togetherCarried := map[string]int{}
	checked := map[int]bool{}
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.TrialCarried:
			carried[e.Seq] = p
			if p.Source.Session == id && p.Condition == machine.Together && p.Outcome == journal.OutcomePass {
				key := fmt.Sprintf("%s/%s/%v/%d", p.Class.Regime, p.Class.Workload, p.Class.Cores, p.Class.DurationS)
				togetherCarried[key]++
			}
		case *journal.TrialIntent:
			if p.Phase == journal.PhaseSearch {
				t.Errorf("live candidate-solo-limit trial at seq %d: %+v", e.Seq, p)
			}
		case *journal.CorePhase:
			if p.From != journal.PhaseSearch || p.To == journal.PhaseSearch {
				continue
			}
			if !strings.Contains(p.Reason, "carried") || !strings.Contains(p.Reason, id) {
				t.Errorf("solo limit completion does not explain its carried source: %+v", p)
			}
			counts := map[machine.Regime]int{}
			for _, seq := range e.Cause {
				if fact := carried[seq]; fact != nil && fact.Source.Session == id && fact.Outcome == journal.OutcomePass && len(fact.Class.Cores) == 1 && fact.Class.Cores[0] == p.Core {
					counts[fact.Class.Regime]++
				}
			}
			for _, regime := range []machine.Regime{machine.R1, machine.R2} {
				if counts[regime] != c.Evidence.Starts() {
					t.Errorf("core %d cites %d carried %s passes, want %d", p.Core, counts[regime], regime, c.Evidence.Starts())
				}
			}
			checked[p.Core] = true
		}
	}
	if !checked[0] || !checked[1] {
		t.Fatalf("candidate solo limits completed: %v", checked)
	}
	if diff := cmp.Diff(firstPassedFullLapLivePasses(t, source), togetherCarried); diff != "" {
		t.Fatalf("source's complete together full-lap evidence was not carried (-source +carried):\n%s", diff)
	}
	if diff := cmp.Diff(firstPassedFullLapLivePasses(t, source), firstPassedFullLapLivePasses(t, events)); diff != "" {
		t.Fatalf("first lap must repeat every live full-lap class despite carried together passes (-source +new):\n%s", diff)
	}
}

func firstPassedFullLapLivePasses(t *testing.T, events []journal.Event) map[string]int {
	t.Helper()
	intents := map[string]*journal.TrialIntent{}
	counts := map[string]int{}
	open := false
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.CheckingLap:
			if p.Event == journal.LapStart {
				if open {
					t.Fatal("first lap restarted before passing in full")
				}
				open = true
			} else if open {
				if !p.Passed || !p.Full {
					t.Fatalf("first lap did not pass in full: %+v", p)
				}
				return counts
			}
		case *journal.TrialIntent:
			if open {
				intents[p.Trial] = p
			}
		case *journal.TrialEnd:
			if intent := intents[p.Trial]; intent != nil {
				if p.Outcome != journal.OutcomePass {
					t.Fatalf("first lap trial did not pass: %+v", p)
				}
				cores := intent.Cores
				if intent.Core != nil {
					cores = []int{*intent.Core}
				}
				key := fmt.Sprintf("%s/%s/%v/%d", intent.Regime, intent.Workload, cores, intent.DurationS)
				counts[key]++
			}
		}
	}
	t.Fatal("no first passed full lap")
	return nil
}

func TestCurrentRulesetStartsHuntFromCarriedTogetherFailure(t *testing.T) {
	t.Parallel()
	cfg := huntConfig(4)
	cfg.Limits[1].Together[6] = -5
	c := config.Default()
	c.CandidateSoloLimits = map[int]int{0: -9, 1: -9, 2: -9, 3: -9}
	_, source, dir := runHunt(t, cfg, nil, func(in *Input) {
		in.Config = c
		in.Until = func(e journal.Event) bool {
			p, ok := e.Data.(*journal.Failure)
			return ok && p.Condition == machine.Together && p.Regime == machine.R7 && p.Attribution == journal.Unattributed
		}
	})
	failure, ok := findPayload(source, func(p *journal.Failure) bool {
		return p.Condition == machine.Together && p.Regime == machine.R7 && p.Attribution == journal.Unattributed
	})
	if !ok {
		t.Fatal("source session has no checking failure together")
	}
	intent, ok := findPayload(source, func(p *journal.TrialIntent) bool { return p.Trial == failure.Trial })
	if !ok || intent.Phase != journal.PhaseChecking {
		t.Fatal("source failure is not a checking step")
	}
	if diff := cmp.Diff([]int{-9, -9, -9, -9}, intent.Profile); diff != "" {
		t.Fatalf("source failing profile (-want +got):\n%s", diff)
	}
	c.CandidateSoloLimits = map[int]int{0: -10, 1: -10, 2: -10, 3: -10}
	id := stampRuleset(t, dir, 6)
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
		Until: func(e journal.Event) bool { return e.Kind == journal.KindHuntStart },
	}); err != nil {
		t.Fatal(err)
	}
	events, torn, err := journal.Read(dir)
	if err != nil || torn != nil {
		t.Fatalf("read transitioned session: %v, torn %q", err, torn)
	}
	if start := events[0].Data.(*journal.SessionStart); start.Ruleset != tuner.Ruleset {
		t.Fatalf("transition ruleset %d, want %d", start.Ruleset, tuner.Ruleset)
	}
	carriedSeq := 0
	found := false
	var currentProfile []int
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.ProfileChange:
			currentProfile = p.To
		case *journal.TrialCarried:
			if p.Source.Session == id && p.Source.Trial == failure.Trial && p.Outcome == journal.OutcomeFailure {
				carriedSeq = e.Seq
			}
		case *journal.TrialIntent:
			if p.Condition == machine.Together && p.Regime == intent.Regime && p.Workload == intent.Workload && p.DurationS == intent.DurationS && cmp.Diff(p.Cores, intent.Cores) == "" && cmp.Diff(p.Profile, intent.Profile) == "" {
				t.Fatalf("carried checking failure together was rerun: %+v", p)
			}
		case *journal.HuntStart:
			if diff := cmp.Diff([]int{-10, -10, -10, -10}, currentProfile); diff != "" {
				t.Fatalf("skipped scheduled profile (-want +got):\n%s", diff)
			}
			if carriedSeq == 0 || p.Failure != carriedSeq || p.Trial != failure.Trial || !strings.Contains(p.Message(), "skipped") {
				t.Fatalf("hunt lost carried failure origin: %+v, carried #%d", p, carriedSeq)
			}
			if diff := cmp.Diff(intent.Profile, p.Failing); diff != "" {
				t.Fatalf("hunt full failing profile (-want +got):\n%s", diff)
			}
			if p.Regime != intent.Regime || p.Workload != intent.Workload || p.DurationS != intent.DurationS || cmp.Diff(intent.Cores, p.Cores) != "" {
				t.Fatalf("hunt changed the failing class: %+v", p)
			}
			found = true
		}
	}
	if !found {
		t.Fatal("transition did not start a hunt")
	}
}
