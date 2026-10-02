package tuner

import (
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestSkipKnownFailure(t *testing.T) {
	for _, tc := range []struct {
		name    string
		live    bool
		profile []int
		passes  int
		change  func(*Trial)
		skip    bool
	}{
		{name: "carried equal", profile: []int{-20, -20}, skip: true},
		{name: "live equal", live: true, profile: []int{-20, -20}, skip: true},
		{name: "shallower", profile: []int{-19, -20}, skip: true},
		{name: "one fewer covering passes", profile: []int{-20, -20}, passes: -1, skip: true},
		{name: "covered", profile: []int{-20, -20}, passes: 1},
		{name: "deeper", profile: []int{-21, -20}},
		{name: "incomparable", profile: []int{-19, -21}},
		{name: "regime", profile: []int{-20, -20}, change: func(tr *Trial) { tr.Regime = machine.R1 }},
		{name: "workload", profile: []int{-20, -20}, change: func(tr *Trial) { tr.Workload = machine.Workloads(machine.R7)[1].ID }},
		{name: "loaded cores", profile: []int{-20, -20}, change: func(tr *Trial) { tr.Cores = []int{0} }},
		{name: "duration", profile: []int{-20, -20}, change: func(tr *Trial) { tr.DurationS++ }},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t, coreStart{phase: journal.PhaseDone, offset: -20}, coreStart{phase: journal.PhaseDone, offset: -20})
			h.add(&journal.ProfileChange{To: []int{-20, -20}})
			tr := Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{1, 0}, DurationS: h.s.durations.StartS, Condition: machine.Resident, Phase: journal.PhaseGuard}
			var seq int
			if tc.live {
				old := tr
				old.Profile = tc.profile
				_, end := h.trial(Action{Kind: RunTrial, Trial: old}, failed)
				seq = end.Seq
				a, ok := h.s.Attribution()
				if !ok {
					t.Fatal("missing live attribution")
				}
				h.decide(a)
			} else {
				seq = h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "old", Trial: "0304"}, Class: journal.TrialClass{Regime: tr.Regime, Workload: tr.Workload, Cores: []int{0, 1}, DurationS: tr.DurationS}, Condition: machine.Resident, Profile: tc.profile, Outcome: journal.OutcomeFailure, Signal: machine.Crash}).Seq
			}
			if tc.passes != 0 {
				count := h.s.n
				if tc.passes < 0 {
					count--
				}
				carryTrials(h, tr.Regime, []int{0, 1}, []int{-20, -20}, tr.DurationS, count, journal.OutcomePass)
			}
			if tc.change != nil {
				tc.change(&tr)
			}
			a := h.s.skipKnownFailure(Action{Kind: RunTrial, Trial: tr})
			if !tc.skip {
				if a.Kind != RunTrial {
					t.Fatalf("unrelated or invalid failure skipped trial: %+v", a)
				}
				return
			}
			p, ok := a.Payload.(*journal.Failure)
			if !ok || p.KnownFailure != seq || !strings.Contains(p.Message(), "skipped") {
				t.Fatalf("missing skip and originating failure #%d: %+v", seq, a)
			}
			if diff := cmp.Diff([]int{seq}, a.Cause); diff != "" {
				t.Fatalf("skip cause (-want +got):\n%s", diff)
			}
		})
	}
}

func TestSkippedResidentFailureMakesProgress(t *testing.T) {
	for _, attributed := range []bool{false, true} {
		name := "hunt"
		if attributed {
			name = "backoff"
		}
		t.Run(name, func(t *testing.T) {
			h := newHarness(t, coreStart{phase: journal.PhaseDone, offset: -20}, coreStart{phase: journal.PhaseDone, offset: -20})
			h.add(&journal.ProfileChange{To: []int{-20, -20}})
			fact := &journal.TrialCarried{Source: journal.FactSource{Session: "old", Trial: "0304"}, Class: journal.TrialClass{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0}, DurationS: h.s.durations.StartS}, Condition: machine.Resident, Profile: []int{-20, -20}, Outcome: journal.OutcomeFailure, Signal: machine.Crash}
			if attributed {
				fact.Core = new(0)
			}
			failure := h.add(fact)
			h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationStart, Steps: []machine.Regime{machine.R7}})
			if attributed {
				h.s.retry = &Trial{Regime: fact.Class.Regime, Workload: fact.Class.Workload, Cores: fact.Class.Cores, DurationS: fact.Class.DurationS, Condition: machine.Resident, Phase: journal.PhaseGuard, Rotation: 1, Retry: true, Profile: fact.Profile}
			}
			found := false
			for range 100 {
				a := h.next()
				if a.Kind == RunTrial {
					t.Fatalf("resident failure was rerun before its decision: %+v", a)
				}
				h.decide(a)
				switch p := a.Payload.(type) {
				case *journal.HuntStart:
					if attributed || p.Failure != failure.Seq || p.Trial != "0304" || !slices.Contains(a.Cause, failure.Seq) {
						t.Fatalf("wrong hunt origin: %+v, cause %v", p, a.Cause)
					}
					if diff := cmp.Diff(fact.Profile, p.Failing); diff != "" {
						t.Fatalf("hunt failing profile (-want +got):\n%s", diff)
					}
					if p.Regime != fact.Class.Regime || p.Workload != fact.Class.Workload || p.DurationS != fact.Class.DurationS || cmp.Diff(fact.Class.Cores, p.Cores) != "" {
						t.Fatalf("hunt lost originating class: %+v", p)
					}
					found = true
				case *journal.TunerDecision:
					if p.Decision == journal.Backoff {
						if !attributed || p.Core != 0 || p.ToOffset != -19 || !slices.Contains(a.Cause, failure.Seq) {
							t.Fatalf("wrong attributed backoff: %+v, cause %v", p, a.Cause)
						}
						found = true
					}
				}
				if found {
					break
				}
			}
			if !found {
				t.Fatal("skipped resident step did not progress to hunt or backoff")
			}
			for range 200 {
				a := h.next()
				if a.Kind == RunTrial {
					return
				}
				h.decide(a)
			}
			t.Fatal("skip loop prevented the next live trial")
		})
	}
}

func TestSkippedRefinementAndRerunFailures(t *testing.T) {
	for _, refining := range []bool{false, true} {
		name := "rerun"
		if refining {
			name = "refinement"
		}
		t.Run(name, func(t *testing.T) {
			h := newHarness(t, coreStart{phase: journal.PhaseResident, offset: -19}, coreStart{phase: journal.PhaseDone, offset: -20})
			h.add(&journal.ProfileChange{To: []int{-19, -20}})
			r := machine.R1
			w := machine.Workloads(r)[0].ID
			d := h.s.durations.StartS
			failure := h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "old", Trial: "0304"}, Class: journal.TrialClass{Regime: r, Workload: w, Cores: []int{0}, DurationS: d}, Condition: machine.Resident, Profile: []int{-20, -20}, Outcome: journal.OutcomeFailure, Signal: machine.Crash, Core: new(0)})
			if refining {
				h.add(&journal.RefineRound{Round: 1, Event: journal.RotationStart, Profile: []int{-20, -20}, Target: []int{-21, -20}, Cores: []int{0}, Starts: h.s.n, StartS: d})
			}
			h.add(&journal.TunerDecision{Core: 0, Phase: journal.PhaseRefine, Decision: journal.Deepen, FromOffset: -19, ToOffset: -20})
			h.add(&journal.ProfileChange{To: []int{-20, -20}})
			var scheduled Action
			if refining {
				scheduled = h.s.roundCheck()
			} else {
				h.s.obligations = []rerun{{class: trialClass{r, w, "[0]", d}, seq: failure.Seq}}
				var ok bool
				scheduled, ok = h.s.rerunNext()
				if !ok {
					t.Fatal("rerun obligation disappeared")
				}
			}
			skip := h.s.skipKnownFailure(scheduled)
			p, ok := skip.Payload.(*journal.Failure)
			if !ok || p.KnownFailure != failure.Seq {
				t.Fatalf("known failure scheduled a live check: %+v", skip)
			}
			h.decide(skip)
			if refining {
				a := h.next()
				end, ok := a.Payload.(*journal.RefineRound)
				if !ok || end.Event != journal.RotationEnd || end.Passed || !slices.Contains(a.Cause, failure.Seq) {
					t.Fatalf("skipped refinement did not close as failed: %+v", a)
				}
				h.decide(a)
			}
			a := h.next()
			backoff, ok := a.Payload.(*journal.TunerDecision)
			if !ok || backoff.Decision != journal.Backoff || backoff.ToOffset != -19 || !slices.Contains(a.Cause, failure.Seq) {
				t.Fatalf("skipped check lost attributed backoff: %+v", a)
			}
			wantPhase := journal.PhaseGuard
			if refining {
				wantPhase = journal.PhaseRefine
			}
			if backoff.Phase != wantPhase {
				t.Fatalf("backoff phase %s, want %s", backoff.Phase, wantPhase)
			}
			h.decide(a)
			assertProjectionReplay(h)
		})
	}
}

func TestKnownIsolatedFailureUsesRecordedOffset(t *testing.T) {
	h := newHarness(t, searchAt(-20)...)
	w := machine.Workloads(machine.R1)[0].ID
	fact := h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "old", Trial: "source"}, Class: journal.TrialClass{Regime: machine.R1, Workload: w, Cores: []int{0}, DurationS: h.s.durations.SearchTrialS}, Condition: machine.Isolated, Profile: []int{-19}, Outcome: journal.OutcomeFailure, Signal: machine.Crash})
	a := h.s.Next()
	p, ok := a.Payload.(*journal.Failure)
	if !ok || p.Attribution != journal.Attributed || p.Core == nil || *p.Core != 0 || p.Offset == nil || *p.Offset != -19 || p.KnownFailure != fact.Seq || cmp.Diff([]int{fact.Seq}, a.Cause) != "" {
		t.Fatalf("isolated skip lost known failure: %+v", a)
	}
}

func TestKnownResidentFailureInfersSoleNonzeroCore(t *testing.T) {
	h := residentHarness(t, 0, -20)
	w := machine.Workloads(machine.R6)[0].ID
	fact := h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "old"}, Class: journal.TrialClass{Regime: machine.R6, Workload: w, Cores: []int{0, 1}, DurationS: 120}, Condition: machine.Resident, Profile: []int{0, -19}, Outcome: journal.OutcomeFailure, Signal: machine.Crash})
	a := h.s.skipKnownFailure(Action{Kind: RunTrial, Trial: Trial{Regime: machine.R6, Workload: w, Cores: []int{0, 1}, DurationS: 120, Condition: machine.Resident}})
	p, ok := a.Payload.(*journal.Failure)
	if !ok || p.Attribution != journal.Attributed || p.Core == nil || *p.Core != 1 || p.Offset == nil || *p.Offset != -19 || p.KnownFailure != fact.Seq {
		t.Fatalf("single nonzero known profile was not attributed: %+v", a)
	}
	h.decide(a)
	back := h.next().Payload.(*journal.TunerDecision)
	if back.Core != 1 || back.ToOffset != -18 || back.FailedMark == nil || *back.FailedMark != -19 {
		t.Fatalf("backoff did not break recorded mark: %+v", back)
	}
}
