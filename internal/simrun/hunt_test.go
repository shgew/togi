package simrun

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"slices"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
)

func huntConfig(cores int) sim.Config {
	model := sim.DefaultModel()
	model.PastEdgeRate = 1
	model.Signals = map[machine.Signal]float64{machine.Crash: 1}
	model.CrashMCE = 0
	edges := make([]sim.Edges, cores)
	for i := range edges {
		edges[i].Isolated = [5]int{-10, -10, -10, -10, -10}
		edges[i].Resident = [7]int{-10, -10, -10, -10, -10, -10, -10}
	}
	return sim.Config{Seed: 1, Cores: cores, Edges: edges, Model: &model}
}

func runHunt(t *testing.T, cfg sim.Config, setup func(*sim.Machine), tweak func(*Input)) (session.Stop, []journal.Event, string) {
	t.Helper()
	m, err := sim.New(cfg)
	if err != nil {
		t.Fatal(err)
	}
	if setup != nil {
		setup(m)
	}
	dir := t.TempDir()
	in := Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Rotations: 1}
	if tweak != nil {
		tweak(&in)
	}
	stop, err := Simulate(context.Background(), in)
	if err != nil {
		t.Fatal(err)
	}
	events, torn, err := journal.Read(dir)
	if err != nil || torn != nil {
		t.Fatalf("read events: %v, torn %q", err, torn)
	}
	return stop, events, dir
}

func findPayload[P journal.Payload](events []journal.Event, accept func(P) bool) (P, bool) {
	var zero P
	for _, e := range events {
		p, ok := e.Data.(P)
		if ok && (accept == nil || accept(p)) {
			return p, true
		}
	}
	return zero, false
}

func TestHuntAllZeroAnchor(t *testing.T) {
	cfg := huntConfig(16)
	cfg.Edges[11].Resident[6] = -5
	stop, events, _ := runHunt(t, cfg, nil, nil)
	if stop.Reason != session.StopRotations {
		t.Fatalf("stop %+v", stop)
	}
	start, ok := findPayload(events, func(p *journal.HuntStart) bool { return p.AnchorSeq == 0 })
	if !ok {
		t.Fatal("no all-zero-anchor hunt")
	}
	if start.AnchorSeq != 0 {
		t.Fatalf("anchor seq %d", start.AnchorSeq)
	}
	end, ok := findPayload(events, func(p *journal.HuntEnd) bool { return p.Hunt == start.Hunt && p.Result == "direct" })
	if !ok || cmp.Diff([]int{11}, end.Cores) != "" {
		t.Fatalf("hunt end %+v, want direct core 11", end)
	}
	if _, ok := findPayload(events, func(p *journal.TunerDecision) bool { return p.Decision == journal.Backoff && p.Core == 11 }); !ok {
		t.Fatal("core 11 not backed off")
	}
	if _, ok := findPayload(events, func(p *journal.TierChange) bool { return p.To == journal.TierBronze }); !ok {
		t.Fatal("no bronze tier")
	}
}

func TestHuntCulpritAfterQualifiedAnchor(t *testing.T) {
	cfg := huntConfig(4)
	for i := range cfg.Edges {
		cfg.Edges[i].Isolated = [5]int{-50, -50, -50, -50, -50}
		cfg.Edges[i].Resident = [7]int{-50, -50, -50, -50, -50, -50, -50}
	}
	cfg.Joints = []sim.Joint{{Members: map[int]int{1: -20}, Regimes: []machine.Regime{machine.R7}, Rate: 10}}
	stop, events, _ := runHunt(t, cfg, nil, func(in *Input) {
		in.Config.CandidateEdges = map[int]int{0: -10, 1: -10, 2: -10, 3: -10}
	})
	if stop.Reason != session.StopRotations {
		t.Fatalf("stop %+v", stop)
	}
	start, ok := findPayload(events, func(p *journal.HuntStart) bool { return p.AnchorSeq > 0 })
	if !ok {
		t.Fatal("no hunt behind qualified anchor")
	}
	end, ok := findPayload(events, func(p *journal.HuntEnd) bool { return p.Hunt == start.Hunt && p.Result == "culprit" })
	if !ok || cmp.Diff([]int{1}, end.Cores) != "" {
		t.Fatalf("hunt end %+v, want culprit core 1", end)
	}
	if _, ok := findPayload(events, func(p *journal.TunerDecision) bool {
		return p.Core == 1 && p.Decision == journal.Backoff && p.FailedMark != nil && p.ToOffset == *p.FailedMark+1
	}); !ok {
		t.Fatal("no one-count-shallower backoff of core 1")
	}
}

func TestHuntJointMark(t *testing.T) {
	cfg := huntConfig(16)
	cfg.Joints = []sim.Joint{{Members: map[int]int{3: -10, 11: -10}, Regimes: []machine.Regime{machine.R7}, Rate: 10}}
	_, events, _ := runHunt(t, cfg, nil, nil)
	mark, ok := findPayload(events, func(p *journal.MarkJoint) bool { return !p.Fallback && len(p.Members) == 2 })
	if !ok || cmp.Diff([]journal.JointMember{{Core: 3, Offset: -10}, {Core: 11, Offset: -10}}, mark.Members) != "" {
		t.Fatalf("joint mark %+v", mark)
	}
	marks, backoffs := 0, 0
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.MarkJoint:
			if p.Mark == mark.Mark {
				marks++
			}
		case *journal.TunerDecision:
			if p.Decision == journal.Backoff && p.Phase == journal.PhaseHunt {
				backoffs++
			}
		case *journal.TrialIntent:
			if marks > 0 && p.Profile[3] <= -10 && p.Profile[11] <= -10 {
				t.Errorf("trial %s reaches J%d", p.Trial, mark.Mark)
			}
		}
	}
	if marks != 1 || backoffs != 1 {
		t.Errorf("mark count %d, hunt backoffs %d, want one each", marks, backoffs)
	}
}

func TestDelayedHuntEscalates(t *testing.T) {
	for _, tc := range []struct {
		name   string
		regime machine.Regime
		after  float64
		long   int
	}{
		{"idle six minutes", machine.R6, 360, 900},
		{"R7 four minutes", machine.R7, 240, 300},
	} {
		t.Run(tc.name, func(t *testing.T) {
			cfg := huntConfig(4)
			cfg.Joints = []sim.Joint{{Members: map[int]int{1: -10}, Regimes: []machine.Regime{tc.regime}, AfterS: tc.after, Rate: 10}}
			_, events, _ := runHunt(t, cfg, nil, nil)
			escalated, full, resolved := false, false, false
			for _, e := range events {
				switch p := e.Data.(type) {
				case *journal.HuntMask:
					if p.Stage == "full" {
						full = true
					}
					if p.Escalated && p.DurationS >= tc.long {
						escalated = true
					}
				case *journal.HuntEnd:
					if (p.Result == "culprit" || p.Result == "direct") && cmp.Diff([]int{1}, p.Cores) == "" {
						resolved = true
					}
				}
			}
			if !full || !escalated || !resolved {
				t.Fatalf("full mask %t, escalation %t, resolved core 1 %t", full, escalated, resolved)
			}
		})
	}
}

func TestResetReasonPowerButtonAndThermal(t *testing.T) {
	for _, tc := range []struct {
		name  string
		reset machine.ResetKind
		want  machine.Signal
		dead  journal.DeadEndCondition
	}{
		{"power button in trial", machine.ResetPowerButton, machine.Crash, ""},
		{"thermal trip", machine.ResetThermalTrip, "", journal.DeadEndThermalTrip},
	} {
		t.Run(tc.name, func(t *testing.T) {
			cfg := huntConfig(4)
			cfg.Script = map[string]sim.Outcome{"0001": {Signal: machine.Crash, Reset: tc.reset}}
			stop, events, _ := runHunt(t, cfg, nil, nil)
			crash, ok := findPayload(events, func(p *journal.CrashDetected) bool { return p.ResetReason == tc.reset })
			if !ok {
				t.Fatalf("no crash with reset reason %s", tc.reset)
			}
			if tc.dead != "" {
				if stop.Reason != session.StopDeadEnd || stop.DeadEnd.Condition != tc.dead {
					t.Fatalf("stop %+v", stop)
				}
				if !crash.Inconclusive {
					t.Fatal("thermal trip recorded as tuning failure")
				}
				return
			}
			if crash.Inconclusive {
				t.Fatal("power button during trial was inconclusive")
			}
			if _, ok := findPayload(events, func(p *journal.Failure) bool { return p.Signal == tc.want }); !ok {
				t.Fatal("power-button crash has no failure")
			}
		})
	}
}

func TestBIOSChangeArchivesAndChecksEdges(t *testing.T) {
	cfg := huntConfig(4)
	m, err := sim.New(cfg)
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	in := Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Rotations: 1}
	if _, err := Simulate(context.Background(), in); err != nil {
		t.Fatal(err)
	}
	old, _, err := journal.Read(dir)
	if err != nil {
		t.Fatal(err)
	}
	id := old[0].Data.(*journal.SessionStart).Session
	m.SetBIOSContext(machine.BIOSContext{BIOSVersion: "new", Board: "sim", CPUModel: "sim", Microcode: "0x2", BoostLimitMHz: 5500})
	m.Reboot()
	if _, err := Simulate(context.Background(), in); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(dir, "archive", id+".jsonl")); err != nil {
		t.Fatal(err)
	}
	events, _, err := journal.Read(dir)
	if err != nil {
		t.Fatal(err)
	}
	carried, ok := findPayload(events, func(p *journal.SessionCarried) bool { return true })
	if !ok || carried.Marks {
		t.Fatalf("carried %+v, want edges only", carried)
	}
	count := 0
	for _, e := range events {
		if p, ok := e.Data.(*journal.CorePhase); ok && p.From == "" {
			if p.To != journal.PhaseSearch || !p.CheckEdge {
				t.Errorf("core %d not checking carried edge: %+v", p.Core, p)
			}
			count++
		}
	}
	if count != 4 {
		t.Errorf("initial core phases %d, want 4", count)
	}
}

func TestJournalUntilCancelsAfterFirstMatchingAppend(t *testing.T) {
	cfg := huntConfig(4)
	stop, events, _ := runHunt(t, cfg, nil, func(in *Input) {
		in.Until = func(e journal.Event) bool { return e.Kind == journal.KindTrialEnd }
	})
	if stop.Reason != session.StopSignal {
		t.Fatalf("stop %+v, want signal", stop)
	}
	ends := 0
	for _, e := range events {
		if e.Kind == journal.KindTrialEnd {
			ends++
		}
	}
	if ends != 1 {
		t.Errorf("trial ends %d, want 1", ends)
	}
}

type crashOnAppend struct {
	session.Journal
	machine *sim.Machine
	at      int
	count   int
}

func (j *crashOnAppend) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := j.Journal.Append(p, cause...)
	if err == nil {
		j.count++
		if j.count == j.at {
			j.machine.NextReset(machine.ResetPowerLoss)
			j.machine.Crash()
			return e, machine.ErrCrashed
		}
	}
	return e, err
}

func TestPowerLossDuringHuntResume(t *testing.T) {
	cfg := huntConfig(4)
	cfg.Joints = []sim.Joint{{Members: map[int]int{1: -10, 3: -10}, Regimes: []machine.Regime{machine.R7}, Rate: 10}}
	start := time.Now()
	_, reference, _ := runHunt(t, cfg, nil, nil)
	at := slices.IndexFunc(reference, func(e journal.Event) bool { return e.Kind == journal.KindHuntStart })
	if at < 0 {
		t.Fatal("no hunt start")
	}
	for _, delta := range []int{1, 3, 10} {
		m, err := sim.New(cfg)
		if err != nil {
			t.Fatal(err)
		}
		dir := t.TempDir()
		wrapped := false
		in := Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Rotations: 1, Wrap: func(j session.Journal) session.Journal {
			if wrapped {
				return j
			}
			wrapped = true
			return &crashOnAppend{Journal: j, machine: m, at: at + delta}
		}}
		if _, err := Simulate(context.Background(), in); err != nil && !errors.Is(err, machine.ErrCrashed) {
			t.Fatal(err)
		}
		events, _, err := journal.Read(dir)
		if err != nil {
			t.Fatal(err)
		}
		got, ok := findPayload(events, func(p *journal.MarkJoint) bool { return p.Mark == 1 })
		want, have := findPayload(reference, func(p *journal.MarkJoint) bool { return p.Mark == 1 })
		if !ok || !have || cmp.Diff(want.Members, got.Members) != "" {
			t.Errorf("append offset %d mark %+v, want %+v", delta, got, want)
		}
		seen := make(map[int]bool)
		for _, e := range events {
			if p, ok := e.Data.(*journal.HuntEnd); ok && p.Hunt == 1 {
				if seen[p.Hunt] {
					t.Error("duplicated hunt.end")
				}
				seen[p.Hunt] = true
			}
		}
	}
	t.Logf("hunt interruption matrix wall time: %s", time.Since(start))
	if time.Since(start) >= 5*time.Second {
		t.Error("hunt interruption matrix exceeded five seconds")
	}
}
