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
	h.add(&journal.ConfigLoaded{Config: cfg})
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
	k := trialClass{machine.R7, machine.Workloads(machine.R7)[0].ID, "[0 1]", 600}
	h.s.obligations = []rerun{{class: k, seq: 1}}
	a := h.next()
	if a.Kind != RunTrial || !a.Trial.Rerun || a.Trial.DurationS != 120 {
		t.Fatalf("rerun starts %+v", a)
	}
	for range h.s.n {
		h.trial(h.next(), passed)
	}
	a = h.next()
	if a.Kind != RunTrial || a.Trial.DurationS != 600 {
		t.Fatalf("long rerun %+v", a)
	}
	h.trial(a, passed)
	if a = h.next(); a.Kind == RunTrial && a.Trial.Rerun {
		t.Fatal("rerun repeated after completion")
	}
}

func TestGuardCarriesDeeperPassAcrossBackoff(t *testing.T) {
	h := residentHarness(t, -20)
	cfg := config.Default()
	cfg.Guard.Rotation = []machine.Regime{machine.R1}
	h.add(&journal.ConfigLoaded{Config: cfg})
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
