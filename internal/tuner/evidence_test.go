package tuner

import (
	"fmt"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestEvidenceValidity(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseDone, offset: -20, fail: new(-21)}, coreStart{phase: journal.PhaseDone, offset: -10, fail: new(-11)})
	w := machine.Workloads(machine.R7)[0].ID
	k := trialClass{machine.R7, w, "[0 1]", 120}
	cases := []struct {
		profile []int
		pass    bool
	}{{[]int{-20, -10}, true}, {[]int{-19, -10}, true}, {[]int{-20, -10}, false}, {[]int{-22, -10}, true}, {[]int{-20, -12}, true}}
	var seqs []int
	for _, tc := range cases {
		tr := Trial{Regime: machine.R7, Workload: w, Cores: []int{0, 1}, DurationS: 120, Profile: tc.profile, Condition: machine.Masked, Phase: journal.PhaseHunt}
		out := passed
		if !tc.pass {
			out = failed
		}
		_, end := h.trial(Action{Kind: RunTrial, Trial: tr}, out)
		seqs = append(seqs, end.Seq)
		if !tc.pass {
			h.decide(h.next())
		}
	}
	if got := h.s.passes(k, []int{-19, -10}, 0); got != 4 {
		t.Errorf("valid passes at shallow profile = %d, want 4", got)
	}
	if got := h.s.passes(k, []int{-21, -10}, 0); got != 1 {
		t.Errorf("deep profile passes after invalidation = %d, want 1", got)
	}
	if got := h.s.passes(k, []int{-20, -11}, 0); got != 1 {
		t.Errorf("mask deep only on second core = %d, want 1", got)
	}
	if !h.s.fails(k, []int{-20, -10}, seqs[1]) {
		t.Error("shallow failure did not invalidate class")
	}
	idle := h.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Condition: machine.Resident, Regime: machine.R6, Profile: []int{-20, -10}})
	idleK := trialClass{machine.R6, machine.Workloads(machine.R6)[0].ID, fmt.Sprint([]int{0, 1}), 900}
	if !h.s.fails(idleK, []int{-20, -10}, 0) {
		t.Fatalf("idle failure #%d did not invalidate R6", idle.Seq)
	}
	masked := h.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Condition: machine.Masked, Regime: machine.R6, Profile: []int{-20, -10}})
	if !h.s.fails(idleK, []int{-20, -10}, idle.Seq) || h.s.queue[len(h.s.queue)-1].class.regime != machine.R6 {
		t.Fatalf("masked idle failure #%d did not enter the R6 wildcard and hunt queue", masked.Seq)
	}
}

func TestMonotonicityWarning(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseDone, offset: -10})
	w := machine.Workloads(machine.R1)[0].ID
	tr := Trial{Regime: machine.R1, Core: 0, Offset: -10, Condition: machine.Resident, Phase: journal.PhaseGuard, DurationS: 120, Workload: w, Profile: []int{-10}}
	var expected []int
	for range h.s.n {
		_, end := h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
		expected = append(expected, end.Seq)
	}
	h.trial(Action{Kind: RunTrial, Trial: tr}, failed)
	a := h.next()
	if p, ok := a.Payload.(*journal.Failure); !ok || p.Attribution != journal.Attributed {
		t.Fatalf("attribution %+v", a)
	}
	h.decide(a)
	a = h.next()
	warning, ok := a.Payload.(*journal.TunerWarning)
	if !ok || cmp.Diff(expected, warning.Passes) != "" {
		t.Fatalf("warning %+v, want passes %v", a, expected)
	}
	h.decide(a)
}

func TestIdleMonotonicityWarning(t *testing.T) {
	for _, tc := range []struct {
		name        string
		passProfile []int
		durations   []int
		cores       []int
		warn        bool
	}{
		{"equal profile", []int{-10, -12}, []int{120}, []int{0, 1}, true},
		{"deeper passing profile", []int{-11, -13}, []int{900}, []int{0, 1}, true},
		{"shallower passing profile", []int{-9, -11}, []int{120}, []int{0, 1}, false},
		{"durations cannot pool", []int{-10, -12}, []int{120, 900}, []int{0, 1}, false},
		{"partial load cannot qualify", []int{-10, -12}, []int{120}, []int{0}, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := residentHarness(t, -10, -12)
			var seqs []int
			for i := range h.s.n {
				tr := Trial{Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Cores: tc.cores, DurationS: tc.durations[i%len(tc.durations)], Condition: machine.Resident, Profile: tc.passProfile}
				_, end := h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
				seqs = append(seqs, end.Seq)
			}
			failure := h.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Condition: machine.Resident, Regime: machine.R6, Profile: []int{-10, -12}})
			a := h.next()
			warning, ok := a.Payload.(*journal.TunerWarning)
			if ok != tc.warn {
				t.Fatalf("idle warning %t, want %t: %+v", ok, tc.warn, a)
			}
			if tc.warn {
				if diff := cmp.Diff(seqs, warning.Passes); diff != "" {
					t.Fatalf("contradicted ends (-want +got):\n%s", diff)
				}
				if diff := cmp.Diff([]int{failure.Seq}, a.Cause); diff != "" {
					t.Fatalf("warning cause (-want +got):\n%s", diff)
				}
				h.decide(a)
			}
			for range 20 {
				a = h.next()
				if start, ok := a.Payload.(*journal.HuntStart); ok {
					if start.Failure != failure.Seq {
						t.Fatalf("warning changed failure to hunt: %+v", start)
					}
					return
				}
				if a.Kind != Decide {
					t.Fatalf("idle failure stopped hunting: %+v", a)
				}
				h.decide(a)
			}
			t.Fatal("idle failure never hunted")
		})
	}
}

func TestIdleMonotonicityChoosesEarliestQualifiedClass(t *testing.T) {
	h := residentHarness(t, -10, -12)
	var expected []int
	for _, duration := range []int{900, 120} {
		tr := Trial{Regime: machine.R6, Cores: []int{0, 1}, DurationS: duration, Condition: machine.Resident, Profile: []int{-10, -12}}
		for i := range h.s.n + 1 {
			_, end := h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
			if duration == 900 && i < h.s.n {
				expected = append(expected, end.Seq)
			}
		}
	}
	for range 20 {
		s := New()
		for _, e := range h.events {
			s.Fold(e)
		}
		failure := journal.Event{Seq: len(h.events) + 1, Data: &journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Condition: machine.Resident, Profile: []int{-10, -12}}}
		s.Fold(failure)
		a := s.Next()
		warning, ok := a.Payload.(*journal.TunerWarning)
		if !ok {
			t.Fatalf("no warning: %+v", a)
		}
		if diff := cmp.Diff(expected, warning.Passes); diff != "" {
			t.Fatalf("warning must cite exactly n earliest-class ends (-want +got):\n%s", diff)
		}
		if diff := cmp.Diff([]int{failure.Seq}, a.Cause); diff != "" {
			t.Fatalf("warning cause (-want +got):\n%s", diff)
		}
	}
}
