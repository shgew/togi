package tuner

import (
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func residentHarness(t *testing.T, offsets ...int) *harness {
	t.Helper()
	starts := make([]coreStart, len(offsets))
	for i, x := range offsets {
		starts[i] = coreStart{phase: journal.PhaseDone, offset: x, fail: new(x - 1), pass: new(x)}
	}
	h := newHarness(t, starts...)
	h.decide(h.next())
	return h
}

func TestCoveredOpenRotationYieldsToRefinement(t *testing.T) {
	offsets := []int{-49, -49, -49, -50}
	for _, tt := range []struct {
		name      string
		qualified []int
		covered   bool
		complete  bool
	}{
		{"same profile", offsets, true, false},
		{"deeper qualified profile", []int{-50, -49, -49, -50}, true, false},
		{"shallower qualified profile", []int{-48, -49, -49, -50}, false, false},
		{"completed same profile", offsets, true, true},
	} {
		t.Run(tt.name, func(t *testing.T) {
			starts := make([]coreStart, len(offsets))
			for i, v := range offsets {
				starts[i] = coreStart{phase: journal.PhaseDone, offset: v}
			}
			h := newHarness(t, starts...)
			for i, pair := range [][]int{{0, 1}, {1, 2}, {2, 3}, {3, 0}, {1, 3}} {
				h.add(&journal.MarkJoint{Mark: i + 1, Hunt: i + 1, Members: []journal.JointMember{{Core: pair[0], Offset: -50}, {Core: pair[1], Offset: -50}}})
			}
			h.add(&journal.ProfileChange{To: tt.qualified})
			h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationStart, Steps: h.s.steps})
			rotation := h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationEnd, Clean: true, Qualifying: true}).Seq
			h.add(&journal.ProfileChange{From: tt.qualified, To: offsets})
			h.add(&journal.GuardRotation{Rotation: 2, Event: journal.RotationStart, Steps: h.s.steps})
			if tt.complete {
				// Replay a fully executed rotation before asking Next to close it.
				// The covered shortcut must only replace work still left to run.
				for a := h.s.rotationNext(); a.Kind == RunTrial; a = h.s.rotationNext() {
					h.trial(a, passed)
				}
			}
			a := h.next()
			end, ok := a.Payload.(*journal.GuardRotation)
			if tt.complete {
				if a.Kind != Decide || !ok || end.Event != journal.RotationEnd || !end.Clean || !end.Qualifying || end.Rotation != 2 {
					t.Fatalf("completed rotation must close clean and qualifying: %+v", a)
				}
				h.decide(a)
				nextRound(h)
				return
			}
			if got := ok && end.Event == journal.RotationEnd && !end.Clean; got != tt.covered {
				t.Fatalf("covered end %t, want %t: %+v", got, tt.covered, a)
			}
			if !tt.covered {
				if a.Kind != RunTrial {
					t.Fatalf("uncovered incomplete rotation must continue testing: %+v", a)
				}
				return
			}
			if a.Cause[0] != rotation {
				t.Fatalf("cause %v, want rotation #%d first", a.Cause, rotation)
			}
			h.decide(a)
			nextRound(h)
		})
	}
}

func TestRotationCoverage(t *testing.T) {
	h := residentHarness(t, -10, -12, -13, -11)
	cases := []struct {
		name    string
		steps   []machine.Regime
		missing []string
	}{{"default", config.Default().Guard.Rotation, nil}, {"short", []machine.Regime{machine.R1, machine.R7}, []string{"R1: 2 more steps", "R2: 3 more steps", "R3: 1 more steps", "R4: 1 more steps", "R5: 1 more steps", "R6: 1 more steps", "R7: 2 more steps"}}}
	for _, tt := range cases {
		t.Run(tt.name, func(t *testing.T) {
			qualified, missing := h.s.qualifying(tt.steps)
			if qualified != (len(tt.missing) == 0) || cmp.Diff(tt.missing, missing) != "" {
				t.Fatalf("qualification %t, missing (-want +got):\n%s", qualified, cmp.Diff(tt.missing, missing))
			}
		})
	}
}

func TestR7StartsAndSharedDuration(t *testing.T) {
	h := residentHarness(t, -10, -11, -12, -13)
	cfg := config.Default()
	cfg.Guard.Rotation = []machine.Regime{machine.R7}
	cfg.Durations.GuardAllCoreS = 480
	h.add(&journal.ConfigLoaded{Config: snapshotConfig(cfg)})
	h.decide(h.next())
	for i := range 4 {
		a := h.next()
		if a.Kind != RunTrial || a.Trial.Regime != machine.R7 || a.Trial.DurationS != 120 || !slices.Equal(a.Trial.Cores, []int{0, 1}) {
			t.Fatalf("start %d: %+v", i, a)
		}
		h.trial(a, passed)
	}
	a := h.next()
	if !slices.Equal(a.Trial.Cores, []int{2, 3}) {
		t.Fatalf("second part %+v", a)
	}
	for range 4 {
		h.trial(h.next(), passed)
	}
	for i := range 4 {
		a = h.next()
		want := 120
		if i == 3 {
			want = 240
		}
		if !slices.Equal(a.Trial.Cores, []int{0, 1, 2, 3}) || a.Trial.DurationS != want {
			t.Fatalf("all cores %+v", a)
		}
		h.trial(a, passed)
	}
	if _, ok := h.next().Payload.(*journal.GuardRotation); !ok {
		t.Fatal("rotation not complete")
	}
}

func TestAttributionAtMaskAnchorAndAlreadyShallower(t *testing.T) {
	h := residentHarness(t, -10, -12)
	profile := []int{-10, -12}
	tr := Trial{Core: 0, Regime: machine.R7, Phase: journal.PhaseHunt, Condition: machine.Masked, Cores: []int{0, 1}, Workload: machine.Workloads(machine.R7)[0].ID, DurationS: 120, Profile: profile, Hunt: 1, Mask: 1}
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(0), DurationS: 10})
	a := h.next()
	f, ok := a.Payload.(*journal.Failure)
	if !ok || *f.Offset != -10 || f.Condition != machine.Masked {
		t.Fatalf("mask failure %+v", a)
	}
	h.decide(a)
	a = h.next()
	d, ok := a.Payload.(*journal.TunerDecision)
	if !ok || d.ToOffset != -9 || d.Phase != journal.PhaseHunt {
		t.Fatalf("backoff %+v", a)
	}
	h.decide(a)
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(0), DurationS: 10})
	h.decide(h.next())
	a = h.next()
	d, ok = a.Payload.(*journal.TunerDecision)
	if !ok || d.ToOffset != -9 || d.FromOffset != -9 {
		t.Fatalf("already shallow %+v", a)
	}
}

func TestRerunLongFailedPart(t *testing.T) {
	h := residentHarness(t, -10, -11)
	tr := Trial{Regime: machine.R7, Cores: []int{0, 1}, Workload: machine.Workloads(machine.R7)[0].ID, Condition: machine.Resident, DurationS: 600}
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(0)})
	first := h.decide(h.next())
	h.decide(h.next())
	h.add(&journal.ProfileChange{From: []int{-10, -11}, To: []int{-9, -11}})
	a := h.next()
	if a.Kind != RunTrial || !a.Trial.Rerun || a.Trial.DurationS != 120 || !slices.Equal(a.Cause, []int{first.Seq}) {
		t.Fatalf("rerun starts %+v", a)
	}
	for range h.s.n {
		h.trial(h.next(), passed)
	}
	a = h.next()
	if a.Kind != RunTrial || a.Trial.DurationS != 600 {
		t.Fatalf("long rerun %+v", a)
	}
	h.trial(a, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(0)})
	failure := h.decide(h.next())
	h.add(&journal.TunerDecision{Core: 0, Phase: journal.PhaseGuard, Decision: journal.Backoff, FromOffset: -9, ToOffset: -8, FailedMark: new(-9)}, failure.Seq)
	h.add(&journal.ProfileChange{From: []int{-9, -11}, To: []int{-8, -11}})
	a, ok := h.s.rerunNext()
	if !ok || a.Trial.DurationS != 600 {
		t.Fatalf("passing starts lost after repeated long-class commitment: %+v", a)
	}
	if diff := cmp.Diff([]int{failure.Seq}, a.Cause); diff != "" {
		t.Fatalf("long rerun cause (-want +got):\n%s", diff)
	}
	h.trial(a, passed)
	for range h.s.n {
		a, ok = h.s.rerunNext()
		if !ok || a.Trial.DurationS != 120 {
			t.Fatalf("new obligation did not demand its passing starts: %+v", a)
		}
		h.trial(a, passed)
	}
	if a, ok = h.s.rerunNext(); ok {
		t.Fatalf("rerun repeated after both obligations completed: %+v", a)
	}
}

func TestRerunRepeatedCommitmentCitesLatestFailure(t *testing.T) {
	h := residentHarness(t, -10)
	tr := Trial{Core: 0, Regime: machine.R1, Workload: machine.Workloads(machine.R1)[0].ID, Phase: journal.PhaseGuard, Condition: machine.Resident, DurationS: 120}
	failed := journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(0)}
	commit := func(a Action, from, to int) int {
		h.trial(a, failed)
		failure := h.decide(h.next())
		h.add(&journal.TunerDecision{Core: 0, Phase: journal.PhaseGuard, Decision: journal.Backoff, FromOffset: from, ToOffset: to, FailedMark: new(from)}, failure.Seq)
		h.add(&journal.ProfileChange{From: []int{from}, To: []int{to}})
		return failure.Seq
	}
	first := commit(Action{Kind: RunTrial, Trial: tr}, -10, -9)
	a, ok := h.s.rerunNext()
	if !ok || !a.Trial.Rerun {
		t.Fatalf("missing first rerun: %+v", a)
	}
	if diff := cmp.Diff([]int{first}, a.Cause); diff != "" {
		t.Fatalf("first cause (-want +got):\n%s", diff)
	}
	second := commit(a, -9, -8)
	k := classOf(h.s.intents["0001"])
	want := []rerun{{class: k, seq: first}, {class: k, seq: second}}
	if diff := cmp.Diff(want, h.s.obligations, cmp.AllowUnexported(rerun{}, trialClass{})); diff != "" {
		t.Fatalf("obligations (-want +got):\n%s", diff)
	}
	a, ok = h.s.rerunNext()
	if !ok || !a.Trial.Rerun {
		t.Fatalf("missing second rerun: %+v", a)
	}
	if diff := cmp.Diff([]int{second}, a.Cause); diff != "" {
		t.Fatalf("second cause (-want +got):\n%s", diff)
	}
	for range h.s.n {
		h.trial(a, passed)
		a, ok = h.s.rerunNext()
	}
	if ok {
		t.Fatalf("rerun repeated after the same number of passes: %+v", a)
	}
}

func TestGuardCarriesDeeperPassAcrossBackoff(t *testing.T) {
	h := residentHarness(t, -20)
	cfg := config.Default()
	cfg.Guard.Rotation = []machine.Regime{machine.R1}
	h.add(&journal.ConfigLoaded{Config: snapshotConfig(cfg)})
	h.decide(h.next())
	first := h.next()
	if first.Kind != RunTrial || first.Trial.Regime != machine.R1 {
		t.Fatalf("first requirement %+v", first)
	}
	h.trial(first, passed)
	h.add(&journal.TunerDecision{Core: 0, Phase: journal.PhaseGuard, Decision: journal.Backoff, FromOffset: -20, ToOffset: -19, FailedMark: new(-20), Reason: "test backoff"})
	h.add(&journal.ProfileChange{From: []int{-20}, To: []int{-19}})
	if a := h.s.rotationNext(); a.Kind != Decide {
		t.Fatalf("deeper passing start was lost after backoff: %+v", a)
	}
}

func TestResidentSingleNonzeroAttribution(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseDone, offset: 0}, coreStart{phase: journal.PhaseResident, offset: -12})
	h.add(&journal.ProfileChange{To: []int{0, -12}})
	tr := Trial{Regime: machine.R6, Cores: []int{0, 1}, Workload: machine.Workloads(machine.R6)[0].ID, Condition: machine.Resident, Phase: journal.PhaseGuard, DurationS: 120}
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash})
	a := h.next()
	f, ok := a.Payload.(*journal.Failure)
	if !ok || f.Attribution != journal.Attributed || *f.Core != 1 || *f.Offset != -12 {
		t.Fatalf("single-nonzero attribution %+v", a)
	}
}

func TestRotationStartInvalidatesProjectedGuard(t *testing.T) {
	h := residentHarness(t, -10)
	h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationStart, Steps: h.s.steps})
	h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationEnd, Clean: true, Qualifying: true})
	closed := projected(h)
	if closed.Guard == nil || closed.Guard.RotationOpen {
		t.Fatalf("expected closed rotation: %+v", closed.Guard)
	}
	h.add(&journal.GuardRotation{Rotation: 2, Event: journal.RotationStart, Steps: h.s.steps})
	open := projected(h)
	if open.Guard == nil || open.Guard.Rotation != 2 || !open.Guard.RotationOpen {
		t.Fatalf("new rotation absent from projection: %+v", open.Guard)
	}
}

func TestResidentMultipleMCECoresRemainUnattributed(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseDone, offset: 0}, coreStart{phase: journal.PhaseResident, offset: -12})
	h.add(&journal.ProfileChange{To: []int{0, -12}})
	tr := Trial{Regime: machine.R7, Cores: []int{0, 1}, Workload: machine.Workloads(machine.R7)[0].ID, Condition: machine.Resident, Phase: journal.PhaseGuard, DurationS: 120}
	intent := h.start(Action{Kind: RunTrial, Trial: tr})
	left := h.add(&journal.MCE{Core: 0, BankType: machine.LoadStore})
	right := h.add(&journal.MCE{Core: 1, BankType: machine.LoadStore})
	h.add(&journal.TrialEnd{Trial: intent.Data.(*journal.TrialIntent).Trial, Outcome: journal.OutcomeFailure, Signal: machine.Crash}, intent.Seq, left.Seq, right.Seq)
	a, ok := h.s.Attribution()
	if !ok {
		t.Fatal("no failure attribution")
	}
	f, ok := a.Payload.(*journal.Failure)
	if !ok || f.Attribution != journal.Unattributed || f.Core != nil || f.Offset != nil {
		t.Fatalf("multiple named MCE cores became attributed: %+v", a)
	}
}

func commitRerunSource(h *harness, kind string) (Trial, journal.Event) {
	h.t.Helper()
	tr := Trial{Regime: machine.R7, Cores: []int{0, 1}, Workload: machine.Workloads(machine.R7)[1].ID, Condition: machine.Resident, DurationS: 600}
	var source journal.Event
	if kind == "idle" {
		source = h.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Condition: machine.Resident, Profile: h.s.Profile()})
		tr.Regime, tr.Workload, tr.Cores, tr.DurationS = machine.R6, machine.Workloads(machine.R6)[0].ID, h.s.ids(), h.s.durations.GuardIdleS
	} else {
		end := journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 5}
		if kind == "attributed" {
			end.Core = new(0)
		}
		h.trial(Action{Kind: RunTrial, Trial: tr}, end)
		source = h.decide(h.next())
	}
	if kind != "attributed" {
		start := h.decide(h.s.huntStartNext()).Data.(*journal.HuntStart)
		if kind == "direct" {
			tr.Workload, tr.Cores, tr.DurationS, tr.Condition = machine.Workloads(machine.R7)[2].ID, h.s.ids(), 240, machine.Masked
			tr.Hunt, tr.Mask = start.Hunt, 1
			h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(1)})
			source = h.decide(h.next())
			h.decide(h.next())
		} else {
			result, cores := "culprit", []int{0}
			if kind == "joint" {
				result, cores = "joint", []int{0, 1}
			}
			h.add(&journal.HuntEnd{Hunt: start.Hunt, Result: result, Cores: cores}, start.Failure)
			if kind == "joint" {
				h.decide(h.next())
			}
		}
	}
	move := h.decide(h.next())
	if p, ok := move.Data.(*journal.TunerDecision); !ok || p.Decision != journal.Backoff {
		h.t.Fatalf("missing commitment: %+v", move)
	}
	h.add(&journal.ProfileChange{From: h.s.Profile(), To: h.s.offsets()}, move.Seq)
	if projected(h).Hunt != nil {
		h.t.Fatal("committed hunt remains active")
	}
	assertProjectionReplay(h)
	return tr, source
}

func TestRerunDerivedCommitments(t *testing.T) {
	for _, kind := range []string{"attributed", "idle", "culprit", "joint", "direct"} {
		t.Run(kind, func(t *testing.T) {
			h := residentHarness(t, -10, -11, -12, -13)
			tr, source := commitRerunSource(h, kind)
			durations := make([]int, h.s.n)
			for i := range durations {
				durations[i] = h.s.durations.StartS
			}
			if tr.DurationS != h.s.durations.StartS {
				durations = append(durations, tr.DurationS)
			}
			for _, duration := range durations {
				a, ok := h.s.rerunNext()
				if !ok || !a.Trial.Rerun || a.Trial.Condition != machine.Resident || a.Trial.Regime != tr.Regime || a.Trial.Workload != tr.Workload || !slices.Equal(a.Trial.Cores, tr.Cores) || a.Trial.DurationS != duration {
					t.Fatalf("derived %s rerun: %+v", kind, a)
				}
				if diff := cmp.Diff([]int{source.Seq}, a.Cause); diff != "" {
					t.Fatalf("rerun cause (-want +got):\n%s", diff)
				}
				h.trial(a, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: duration})
			}
			if a, ok := h.s.rerunNext(); ok {
				t.Fatalf("completed obligation repeats: %+v", a)
			}
		})
	}
}

func TestRerunFIFOAndSharedDuration(t *testing.T) {
	h := residentHarness(t, -10, -11, -12, -13)
	var causes []int
	for _, tc := range []struct {
		core     int
		regime   machine.Regime
		duration int
	}{{0, machine.R1, 120}, {1, machine.R2, 600}} {
		tr := Trial{Core: tc.core, Regime: tc.regime, Workload: machine.Workloads(tc.regime)[1].ID, Condition: machine.Resident, DurationS: tc.duration}
		h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(tc.core)})
		failure := h.decide(h.next())
		causes = append(causes, failure.Seq)
		move := h.decide(h.next())
		h.add(&journal.ProfileChange{From: h.s.Profile(), To: h.s.offsets()}, move.Seq)
	}
	for i, regime := range []machine.Regime{machine.R1, machine.R2} {
		count := h.s.n
		if i == 1 {
			count++
		}
		for start := range count {
			a, ok := h.s.rerunNext()
			duration := 120
			if start == h.s.n {
				duration = 600
			}
			if !ok || a.Trial.Core != i || a.Trial.Regime != regime || a.Trial.Workload != machine.Workloads(regime)[1].ID || a.Trial.DurationS != duration || !slices.Equal(a.Cause, []int{causes[i]}) {
				t.Fatalf("FIFO obligation %d start %d: %+v", i, start, a)
			}
			h.trial(a, passed)
		}
	}
	if a, ok := h.s.rerunNext(); ok {
		t.Fatalf("shared duration added redundant long start: %+v", a)
	}
}
