package simrun

import (
	"bytes"
	"context"
	"fmt"
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

func stopAfterIsolatedPasses(in *Input, cores int) {
	trials := map[string]int{}
	passed := map[int]bool{}
	in.Until = func(e journal.Event) bool {
		switch p := e.Data.(type) {
		case *journal.TrialIntent:
			if p.Condition == machine.Isolated && p.Core != nil {
				trials[p.Trial] = *p.Core
			}
		case *journal.TrialEnd:
			if p.Outcome == journal.OutcomePass {
				if core, ok := trials[p.Trial]; ok {
					passed[core] = true
				}
			}
		}
		return len(passed) == cores
	}
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
	t.Parallel()
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
	t.Parallel()
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

func TestAnchorOffsetBackendFailureRaisesTheAnchor(t *testing.T) {
	t.Parallel()
	cfg := huntConfig(4)
	for i := range cfg.Edges {
		cfg.Edges[i].Isolated = [5]int{-50, -50, -50, -50, -50}
		cfg.Edges[i].Resident = [7]int{-50, -50, -50, -50, -50, -50, -50}
	}
	cfg.Joints = []sim.Joint{{Members: map[int]int{1: -20}, Regimes: []machine.Regime{machine.R7}, Rate: 10}}
	candidates := func(in *Input) {
		in.Config.CandidateEdges = map[int]int{0: -10, 1: -10, 2: -10, 3: -10}
	}
	_, probe, _ := runHunt(t, cfg, nil, candidates)
	first, ok := findPayload(probe, func(p *journal.HuntStart) bool { return p.AnchorSeq > 0 })
	if !ok {
		t.Fatal("no qualified anchor")
	}
	var mask *journal.HuntMask
	var trial string
	for _, e := range probe {
		switch p := e.Data.(type) {
		case *journal.HuntMask:
			if p.Hunt == first.Hunt && mask == nil && len(p.Cores) < len(first.Candidates) {
				mask = p
			}
		case *journal.TrialIntent:
			if mask != nil && p.Hunt == first.Hunt && p.Mask == mask.Mask {
				trial = p.Trial
			}
		}
		if trial != "" {
			break
		}
	}
	if mask == nil || trial == "" {
		t.Fatal("no partially masked trial")
	}
	held := -1
	for _, core := range first.Candidates {
		if !slices.Contains(mask.Cores, core) && first.Anchor[core] != 0 {
			held = core
			break
		}
	}
	if held < 0 {
		t.Fatalf("no held core at a nonzero anchor: %+v", mask)
	}
	cfg.Script = map[string]sim.Outcome{trial: {Signal: machine.ComputationError, Core: held, AtS: 1}}
	_, events, _ := runHunt(t, cfg, nil, candidates)
	end, ok := findPayload(events, func(p *journal.HuntEnd) bool {
		return p.Hunt == first.Hunt && p.Result == "direct" && cmp.Diff([]int{held}, p.Cores) == ""
	})
	if !ok {
		t.Fatalf("hunt did not directly attribute held core %d, end %+v", held, end)
	}
	if _, ok := findPayload(events, func(p *journal.TunerDecision) bool {
		return p.Core == held && p.Decision == journal.Backoff && p.FailedMark != nil && *p.FailedMark == first.Anchor[held]
	}); !ok {
		t.Fatalf("no failed mark at core %d anchor %d", held, first.Anchor[held])
	}
	next, ok := findPayload(events, func(p *journal.HuntStart) bool { return p.Hunt > first.Hunt })
	if !ok || next.AnchorSeq != first.AnchorSeq {
		t.Fatalf("next hunt %+v, want the anchor of #%d raised", next, first.AnchorSeq)
	}
	if next.Anchor[held] <= first.Anchor[held] || next.Anchor[held] != next.Failing[held] {
		t.Fatalf("next anchor %v, want core %d raised past its mark %d to the failing offset %d", next.Anchor, held, first.Anchor[held], next.Failing[held])
	}
	if slices.Contains(next.Candidates, held) {
		t.Fatalf("next candidates %v include core %d, which is no deeper than the raised anchor", next.Candidates, held)
	}
}

func TestHuntJointMark(t *testing.T) {
	t.Parallel()
	cfg := huntConfig(16)
	model := sim.DefaultModel()
	model.PastEdgeRate = 1
	cfg.Model = &model
	for i := range cfg.Edges {
		cfg.Edges[i].Isolated = [5]int{-50, -50, -50, -50, -50}
		cfg.Edges[i].Resident = [7]int{-50, -50, -50, -50, -50, -50, -50}
	}
	cfg.Joints = []sim.Joint{{Members: map[int]int{3: -10, 11: -10}, Regimes: []machine.Regime{machine.R7}, Rate: 10}}
	stop, events, _ := runHunt(t, cfg, nil, nil)
	if stop.Reason != session.StopRotations {
		t.Fatalf("stop %+v", stop)
	}
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
			if p.FailedMark != nil {
				t.Errorf("joint crash produced single-core mark: %+v", p)
			}
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

func TestSharedVoltageJointBacksOffOnlyTheShallowestCore(t *testing.T) {
	t.Parallel()
	cfg := huntConfig(8)
	edges := []int{-31, -38, -37, -34, -40, -40, -40, -40}
	for i, edge := range edges {
		cfg.Edges[i].Isolated = [5]int{edge, edge, edge, edge, edge}
		cfg.Edges[i].Resident = [7]int{edge, edge, edge, edge, edge, edge, edge}
	}
	ccd0 := func(offset int) map[int]int { return map[int]int{0: offset, 1: offset, 2: offset, 3: offset} }
	cfg.Joints = []sim.Joint{
		{Members: ccd0(-27), Regimes: []machine.Regime{machine.R7}, Rate: 0.05},
		{Members: ccd0(-23), Regimes: []machine.Regime{machine.R7}, Rate: 0.0009},
	}
	stop, events, _ := runHunt(t, cfg, nil, nil)
	if stop.Reason != session.StopRotations {
		t.Fatalf("stop %+v", stop)
	}
	hunts, crashes := 0, 0
	var final []int
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.HuntStart:
			hunts++
		case *journal.CrashDetected:
			crashes++
		case *journal.ProfileChange:
			final = p.To
		}
	}
	if hunts > 8 || crashes > 40 {
		t.Errorf("%d hunts and %d crashes, want at most 8 and 40", hunts, crashes)
	}
	want := slices.Clone(edges)
	want[0] = -22
	if diff := cmp.Diff(want, final); diff != "" {
		t.Errorf("final profile (-want +got):\n%s", diff)
	}
}

func TestHuntJointMisleadingMCE(t *testing.T) {
	t.Parallel()
	cfg := huntConfig(16)
	model := sim.DefaultModel()
	model.PastEdgeRate = 1
	cfg.Model = &model
	cfg.Joints = []sim.Joint{{Members: map[int]int{3: -10, 11: -10}, Regimes: []machine.Regime{machine.R7}, Rate: 10, CrashMCECore: new(3)}}
	stop, events, _ := runHunt(t, cfg, nil, func(in *Input) {
		in.Until = func(e journal.Event) bool {
			p, ok := e.Data.(*journal.TunerDecision)
			return ok && p.Core == 3 && p.FailedMark != nil && *p.FailedMark == -10
		}
	})
	if stop.Reason != session.StopSignal {
		t.Fatalf("stop %+v", stop)
	}
	if _, ok := findPayload(events, func(p *journal.TunerDecision) bool {
		return p.Core == 3 && p.FailedMark != nil && *p.FailedMark == -10
	}); !ok {
		t.Fatal("misleading joint MCE did not attribute core 3 at -10")
	}
	if mark, ok := findPayload[*journal.MarkJoint](events, nil); ok {
		t.Fatalf("misleading core-local evidence produced joint mark: %+v", mark)
	}
}

func TestDelayedHuntEscalates(t *testing.T) {
	t.Parallel()
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
			fullPasses, escalated, resolved := 0, false, false
			fullTrials := map[string]bool{}
			fullMask := map[[2]int]bool{}
			for _, e := range events {
				switch p := e.Data.(type) {
				case *journal.HuntMask:
					if p.Stage == "full" && !p.Escalated {
						fullMask[[2]int{p.Hunt, p.Mask}] = true
					}
					if p.Escalated && p.DurationS == tc.long {
						escalated = true
					}
				case *journal.TrialIntent:
					if p.Hunt > 0 && fullMask[[2]int{p.Hunt, p.Mask}] {
						fullTrials[p.Trial] = true
					}
				case *journal.TrialEnd:
					if fullTrials[p.Trial] && p.Outcome == journal.OutcomePass {
						fullPasses++
					}
				case *journal.HuntEnd:
					if (p.Result == "culprit" || p.Result == "direct") && cmp.Diff([]int{1}, p.Cores) == "" {
						resolved = true
					}
				}
			}
			if fullPasses != 5 || !escalated || !resolved {
				t.Fatalf("full passes %d, escalation %t, resolved core 1 %t", fullPasses, escalated, resolved)
			}
		})
	}
}

func TestFlatFailureRestartsSilverTierClock(t *testing.T) {
	t.Parallel()
	cfg := huntConfig(4)
	cfg.Edges[2].Flat = 1 / (30 * 3600.0)
	c := config.Default()
	c.Durations.GuardTrialS = 3600
	c.Durations.GuardIdleS = 3600
	c.Durations.GuardAllCoreS = 3600
	silver := false
	failed := false
	_, events, dir := runHunt(t, cfg, nil, func(in *Input) {
		in.Config = c
		in.Rotations = 0
		in.Until = func(e journal.Event) bool {
			switch p := e.Data.(type) {
			case *journal.TierChange:
				silver = silver || p.To == journal.TierSilver
			case *journal.Failure:
				failed = failed || silver && p.Attribution == journal.Unattributed
			case *journal.ProfileChange:
				return failed
			}
			return false
		}
	})
	if !silver || !failed {
		t.Fatalf("silver tier %t, subsequent failure %t", silver, failed)
	}
	var failureSeq int
	seenSilver := false
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.TierChange:
			if failureSeq > 0 && (p.To == journal.TierSilver || p.To == journal.TierGold) {
				t.Errorf("tier %s at #%d reuses pre-failure exposure", p.To, e.Seq)
			}
			seenSilver = seenSilver || p.To == journal.TierSilver
		case *journal.Failure:
			if seenSilver && failureSeq == 0 {
				failureSeq = e.Seq
			}
		}
	}
	if failureSeq == 0 {
		t.Fatal("no failure after silver")
	}
	state, err := journal.ReadState(dir)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(failureSeq, state.Guard.TierClockSeq); diff != "" {
		t.Errorf("tier clock (-want +got):\n%s", diff)
	}
	if state.Guard.CleanS >= 24*3600 || state.Guard.RateBoundPerH != nil && *state.Guard.RateBoundPerH <= 3.0/24 {
		t.Errorf("post-failure exposure includes old hours: %+v", state.Guard)
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

type resetAtEvent struct {
	session.Journal
	machine *sim.Machine
	kind    journal.Kind
	reset   machine.ResetKind
	fired   *bool
}

func (j *resetAtEvent) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := j.Journal.Append(p, cause...)
	if err == nil && !*j.fired && e.Kind == j.kind {
		*j.fired = true
		j.machine.NextReset(j.reset)
		j.machine.Crash()
		return e, machine.ErrCrashed
	}
	return e, err
}

func TestThermalTripWhileTrialRunningDeadEnds(t *testing.T) {
	t.Parallel()
	fired := false
	var m *sim.Machine
	stop, events, _ := runHunt(t, huntConfig(4), func(machine *sim.Machine) { m = machine }, func(in *Input) {
		in.Wrap = func(j session.Journal) session.Journal {
			return &resetAtEvent{Journal: j, machine: m, kind: journal.KindTrialStart, reset: machine.ResetThermalTrip, fired: &fired}
		}
	})
	if !fired || stop.Reason != session.StopDeadEnd || stop.DeadEnd == nil || stop.DeadEnd.Condition != journal.DeadEndThermalTrip {
		t.Fatalf("thermal trip fired %t, stop %+v", fired, stop)
	}
	if p, ok := findPayload(events, func(p *journal.CrashDetected) bool {
		return p.ResetReason == machine.ResetThermalTrip && p.Inconclusive
	}); !ok || p.InFlight == nil {
		t.Fatalf("missing in-flight thermal crash: %+v", p)
	}
	if _, ok := findPayload(events, func(p *journal.Failure) bool { return true }); ok {
		t.Fatal("thermal trip recorded as tuning failure")
	}
}

func TestPowerButtonBetweenTrialsIsInconclusive(t *testing.T) {
	t.Parallel()
	fired := false
	var m *sim.Machine
	_, events, _ := runHunt(t, huntConfig(4), func(machine *sim.Machine) { m = machine }, func(in *Input) {
		in.Wrap = func(j session.Journal) session.Journal {
			return &resetAtEvent{Journal: j, machine: m, kind: journal.KindTrialEnd, reset: machine.ResetPowerButton, fired: &fired}
		}
		in.Until = func(e journal.Event) bool {
			p, ok := e.Data.(*journal.CrashDetected)
			return ok && p.ResetReason == machine.ResetPowerButton
		}
	})
	if !fired {
		t.Fatal("no between-trials power-button crash")
	}
	crash, ok := findPayload(events, func(p *journal.CrashDetected) bool { return p.ResetReason == machine.ResetPowerButton })
	if !ok || !crash.Inconclusive {
		t.Fatalf("power-button crash %+v, want inconclusive", crash)
	}
	if _, ok := findPayload(events, func(p *journal.Failure) bool { return true }); ok {
		t.Fatal("power-button crash between trials recorded a tuning failure")
	}
}

type jumpAtTrialStart struct {
	session.Journal
	machine *sim.Machine
	jumped  bool
}

func (j *jumpAtTrialStart) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := j.Journal.Append(p, cause...)
	if err == nil && !j.jumped && e.Kind == journal.KindTrialStart {
		j.machine.JumpWall(-2 * time.Hour)
		j.jumped = true
	}
	return e, err
}

func TestClockJumpDoesNotLoseTrialMCE(t *testing.T) {
	t.Parallel()
	cfg := huntConfig(4)
	cfg.Script = map[string]sim.Outcome{"0001": {Signal: machine.CorrectedMCE, AtS: 2, Core: 0}}
	m, err := sim.New(cfg)
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	jump := &jumpAtTrialStart{machine: m}
	in := Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Rotations: 1,
		Wrap:  func(j session.Journal) session.Journal { jump.Journal = j; return jump },
		Until: func(e journal.Event) bool { return e.Kind == journal.KindTrialEnd },
	}
	if _, err := Simulate(context.Background(), in); err != nil {
		t.Fatal(err)
	}
	events, _, err := journal.Read(dir)
	if err != nil {
		t.Fatal(err)
	}
	if !jump.jumped {
		t.Fatal("wall clock was not moved after trial start")
	}
	start, ok := findPayload(events, func(p *journal.TrialIntent) bool { return p.Trial == "0001" })
	if !ok {
		t.Fatal("missing scripted trial")
	}
	end, ok := findPayload(events, func(p *journal.TrialEnd) bool { return p.Trial == start.Trial })
	if !ok || end.Outcome != journal.OutcomeFailure || end.Signal != machine.CorrectedMCE {
		t.Fatalf("trial end %+v, want corrected MCE failure", end)
	}
	if _, ok := findPayload(events, func(p *journal.MCE) bool { return p.Corrected }); !ok {
		t.Fatal("corrected machine check was lost after the wall clock jump")
	}
}

func TestBIOSChangeArchivesAndChecksEdges(t *testing.T) {
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
	old, _, err := journal.Read(dir)
	if err != nil {
		t.Fatal(err)
	}
	id := old[0].Data.(*journal.SessionStart).Session
	m.SetBIOSContext(machine.BIOSContext{BIOSVersion: "new", Board: "sim", CPUModel: "sim", Microcode: "0x2", BoostLimitMHz: 5500})
	m.Reboot()
	phases := 0
	in.Until = func(e journal.Event) bool {
		if p, ok := e.Data.(*journal.CorePhase); ok && p.From == "" {
			phases++
		}
		return phases == 4
	}
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

func TestPowerLossDuringHuntResume(t *testing.T) {
	t.Parallel()
	cfg := huntConfig(4)
	cfg.Joints = []sim.Joint{{Members: map[int]int{1: -10, 3: -10}, Regimes: []machine.Regime{machine.R7}, Rate: 10}}
	runInterruptionMatrix(t, "hunt", cfg, quickMatrixConfig(),
		func(e journal.Event) bool { return e.Kind == journal.KindHuntStart },
		func(e journal.Event, closing *bool) bool {
			if e.Kind == journal.KindHuntEnd {
				*closing = true
			}
			return *closing && e.Kind == journal.KindProfileChange
		})
}

func TestPowerLossDuringRefineResume(t *testing.T) {
	t.Parallel()
	cfg := huntConfig(4)
	for i := range cfg.Edges {
		cfg.Edges[i].Isolated = [5]int{-50, -50, -50, -50, -50}
		cfg.Edges[i].Resident = [7]int{-50, -50, -50, -50, -50, -50, -50}
	}
	c := quickMatrixConfig()
	runInterruptionMatrix(t, "refine", cfg, c,
		func(e journal.Event) bool {
			p, ok := e.Data.(*journal.RefineRound)
			return ok && p.Event == journal.RotationStart
		},
		func(e journal.Event, _ *bool) bool {
			p, ok := e.Data.(*journal.RefineRound)
			return ok && p.Event == journal.RotationEnd
		})
}

func quickMatrixConfig() config.Config {
	c := config.Default()
	c.CandidateEdges = map[int]int{0: -10, 1: -10, 2: -10, 3: -10}
	c.Evidence.Miss = .5
	c.Durations.SearchTrialS = 1
	c.Durations.StartS = 1
	c.Durations.GuardTrialS = 1
	c.Durations.GuardIdleS = 1
	c.Durations.GuardAllCoreS = 1
	return c
}

func runInterruptionMatrix(t *testing.T, name string, cfg sim.Config, c config.Config, open func(journal.Event) bool, closeWindow func(journal.Event, *bool) bool) {
	t.Helper()
	_, prefix, _ := runHunt(t, cfg, nil, func(in *Input) {
		in.Config = c
		in.Rotations = 0
		in.Until = open
	})
	openIndex := slices.IndexFunc(prefix, open)
	if openIndex < 0 {
		t.Fatalf("%s prefix missing opening event", name)
	}
	prefix = prefix[:openIndex+1]
	lines := make([][]byte, len(prefix))
	for i, e := range prefix {
		lines[i] = e.Raw
	}
	snapshot := map[string][]byte{"events.jsonl": append(bytes.Join(lines, []byte{'\n'}), '\n')}
	var boots []string
	reasons := map[string]machine.ResetKind{}
	for _, e := range prefix {
		if !slices.Contains(boots, e.Boot) {
			boots = append(boots, e.Boot)
		}
		if p, ok := e.Data.(*journal.CrashDetected); ok {
			reasons[e.Boot] = p.ResetReason
		}
	}
	run := func(t *testing.T, at int) []journal.Event {
		t.Helper()
		dir := t.TempDir()
		for path, data := range snapshot {
			if err := os.WriteFile(filepath.Join(dir, path), data, 0o600); err != nil {
				t.Fatal(err)
			}
		}
		resumed, err := sim.Resume(dir, cfg)
		if err != nil {
			t.Fatal(err)
		}
		resumed.Boots = 0
		m, err := sim.New(resumed)
		if err != nil {
			t.Fatal(err)
		}
		for i := range boots {
			kind := machine.ResetPowerLoss
			if i+1 < len(boots) {
				kind = reasons[boots[i+1]]
			}
			m.NextReset(kind)
			m.Reboot()
		}
		closing, closed := false, false
		crashed := false
		in := Input{Config: c, ConfigPath: config.DefaultPath, Dir: dir, Machine: m,
			Until: func(e journal.Event) bool {
				if closed {
					return at < 0 || crashed
				}
				closed = closeWindow(e, &closing)
				return closed && at < 0
			},
		}
		if at >= 0 {
			in.Wrap = func(j session.Journal) session.Journal {
				if crashed {
					return j
				}
				if at == 0 {
					crashed = true
					m.NextReset(machine.ResetPowerLoss)
					m.Crash()
					return j
				}
				return &matrixCrash{Journal: j, machine: m, at: at, crashed: &crashed}
			}
		}
		stop, err := Simulate(context.Background(), in)
		if err != nil {
			t.Fatalf("%s append %d: %v", name, at, err)
		}
		if stop.Reason != session.StopSignal || !closed {
			t.Fatalf("%s append %d: stop %+v deadend %+v, closing %t", name, at, stop, stop.DeadEnd, closed)
		}
		if at >= 0 && !crashed {
			t.Fatalf("%s append %d did not crash", name, at)
		}
		events, torn, err := journal.Read(dir)
		if err != nil || torn != nil {
			t.Fatalf("%s append %d: journal read %v, torn %q", name, at, err, torn)
		}
		return events[len(prefix):]
	}
	reference := run(t, -1)
	if len(reference) == 0 {
		t.Fatal("empty reference window")
	}
	want := matrixCommitments(reference)
	t.Logf("%s interruption matrix: %d crash points", name, len(reference)+2)
	for at := 0; at <= len(reference)+1; at++ {
		t.Run(fmt.Sprint(at), func(t *testing.T) {
			t.Parallel()
			events := run(t, at)
			got := matrixCommitments(events)
			if diff := cmp.Diff(want, got); diff != "" {
				if at == 0 {
					t.Fatalf("%s before first append commitments (-want +got):\n%s", name, diff)
				}
				t.Fatalf("%s append %d/%d at %s commitments (-want +got):\n%s", name, at, len(reference), reference[min(at-1, len(reference)-1)].Kind, diff)
			}
			seen := map[[2]int]bool{}
			for _, e := range events {
				if p, ok := e.Data.(*journal.HuntMask); ok {
					key := [2]int{p.Hunt, p.Mask}
					if seen[key] {
						t.Fatalf("%s append %d: duplicated hunt.mask %v", name, at, key)
					}
					seen[key] = true
				}
			}
		})
	}
}

type matrixCrash struct {
	session.Journal
	machine *sim.Machine
	at      int
	count   int
	crashed *bool
}

func (j *matrixCrash) Append(p journal.Payload, cause ...int) (journal.Event, error) {
	e, err := j.Journal.Append(p, cause...)
	if err == nil {
		j.count++
		if j.count == j.at {
			*j.crashed = true
			j.machine.NextReset(machine.ResetPowerLoss)
			j.machine.Crash()
			return e, machine.ErrCrashed
		}
	}
	return e, err
}

type matrixResult struct {
	Marks    [][]journal.JointMember
	Backoffs [][3]int
	Profile  []int
	Ends     []string
	Rounds   []string
}

func matrixCommitments(events []journal.Event) matrixResult {
	var out matrixResult
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.MarkJoint:
			out.Marks = append(out.Marks, p.Members)
		case *journal.TunerDecision:
			if p.Decision == journal.Backoff && p.Phase == journal.PhaseHunt {
				out.Backoffs = append(out.Backoffs, [3]int{p.Core, p.FromOffset, p.ToOffset})
			}
		case *journal.HuntEnd:
			out.Ends = append(out.Ends, fmt.Sprintf("%d:%s:%v", p.Hunt, p.Result, p.Cores))
		case *journal.RefineRound:
			if p.Event == journal.RotationEnd {
				out.Rounds = append(out.Rounds, fmt.Sprintf("%d:%t", p.Round, p.Passed))
			}
		case *journal.ProfileChange:
			out.Profile = p.To
		}
	}
	return out
}
