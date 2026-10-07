package tuner

import (
	"fmt"
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

const zeroCarrySource = "20261005T000000Z"

var zeroCarryClass = journal.TrialClass{Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Cores: []int{0, 1, 2, 3}, DurationS: 120}

func zeroCarryFact(seq int, trial string, profile []int, rerun bool) *journal.TrialCarried {
	return &journal.TrialCarried{Source: journal.FactSource{Session: zeroCarrySource, Seq: seq, Trial: trial}, Class: zeroCarryClass, Condition: machine.Parked, Phase: journal.PhaseHunt, Rerun: rerun, Profile: profile, Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 40}
}

// zeroCarryHarness starts a session the way a transition does: carried facts, the carry commitment with core 1's
// failure point at CO 0 from the source failure at failureSeq, then each core's first phase.
func zeroCarryHarness(t *testing.T, failureSeq int, facts ...journal.Payload) (*harness, []int) {
	t.Helper()
	h := &harness{t: t, s: New()}
	infos := make([]machine.CoreInfo, 4)
	for i := range infos {
		infos[i] = machine.CoreInfo{Core: i, CCD: i / 2, CPUs: []int{i, i + 4}}
	}
	begin := h.add(&journal.SessionStart{Schema: journal.Schema, Session: "s", Cores: infos})
	h.add(&journal.ConfigLoaded{Path: config.DefaultPath, Config: snapshotConfig(config.Default())})
	var seqs []int
	for _, f := range facts {
		seqs = append(seqs, h.add(f).Seq)
	}
	carried := h.add(&journal.SessionCarried{Sources: []journal.CarriedSource{{Session: zeroCarrySource, Ruleset: 9, Schema: journal.Schema}}, FailurePoints: true, Carried: []journal.CarriedCore{{Core: 1, FailurePoint: new(0), FailurePointSession: zeroCarrySource, FailurePointSeq: failureSeq, FailurePointSignal: machine.Crash}}})
	for i := range infos {
		p := &journal.CorePhase{Core: i, To: journal.PhaseSearch, Reason: "baseline"}
		if i == 1 {
			p.FailurePoint = new(0)
		}
		seqs = append(seqs, h.add(p, begin.Seq, carried.Seq).Seq)
	}
	return h, seqs
}

func TestCarriedFailurePointAtZeroRerunsAllZero(t *testing.T) {
	for _, rerunFails := range []bool{false, true} {
		t.Run(fmt.Sprint(rerunFails), func(t *testing.T) {
			older := zeroCarryFact(20, "0005", []int{-10, -10, -20, -25}, false)
			decisive := zeroCarryFact(40, "0012", []int{0, 0, -20, -25}, false)
			later := zeroCarryFact(50, "0013", []int{-10, -10, -20, -25}, false)
			h, seqs := zeroCarryHarness(t, 43, older, decisive, later)
			fact, phase := seqs[1], seqs[4]
			a := h.next()
			want := Trial{Regime: machine.R6, Workload: zeroCarryClass.Workload, Cores: []int{0, 1, 2, 3}, DurationS: 120, Condition: machine.Parked, Phase: journal.PhaseHunt, Profile: []int{0, 0, 0, 0}, Rerun: true}
			if diff := cmp.Diff(Action{Kind: RunTrial, Trial: want, Cause: []int{fact}}, a); diff != "" {
				t.Fatalf("all-zero rerun of the carried failure (-want +got):\n%s", diff)
			}
			if !rerunFails {
				h.trial(a, passed)
				if c := h.s.core(1); c.fail != nil {
					t.Fatalf("failure point %d stands after a passed all-zero rerun", *c.fail)
				}
				if a := h.next(); a.Kind == Decide {
					if dead, ok := a.Payload.(*journal.DeadEnd); ok {
						t.Fatalf("dead end after a passed all-zero rerun: %+v", dead)
					}
				}
				assertProjectionReplay(h)
				return
			}
			_, rerun := h.trial(a, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash})
			h.decide(h.next())
			a = h.next()
			dead, ok := a.Payload.(*journal.DeadEnd)
			if !ok || dead.Condition != journal.DeadEndFailureAtZero || dead.Core == nil || *dead.Core != 1 || !slices.Equal(a.Cause, []int{phase, fact, rerun.Seq}) {
				t.Fatalf("dead end %+v", a)
			}
			wantDetail := fmt.Sprintf("core 01 has a failure point at CO 0 carried from session %s; the rerun with every core at CO 0 failed too (#%d), so the instability is not caused by Curve Optimizer", zeroCarrySource, rerun.Seq)
			if diff := cmp.Diff(wantDetail, dead.Detail); diff != "" {
				t.Fatal(diff)
			}
			assertProjectionReplay(h)
		})
	}
}

func TestCarriedFailurePointAtZeroDeadEndsAtOnceWhenConfirmed(t *testing.T) {
	for _, tc := range []struct {
		name  string
		facts []journal.Payload
		cause func(seqs []int) []int
		rerun bool
	}{
		{
			name:  "failing profile all at CO 0",
			facts: []journal.Payload{zeroCarryFact(40, "0012", []int{0, 0, 0, 0}, false)},
			cause: func(seqs []int) []int { return []int{seqs[2], seqs[0]} },
		},
		{
			name:  "failed all-zero rerun carried",
			facts: []journal.Payload{zeroCarryFact(40, "0012", []int{0, 0, -20, -25}, false), zeroCarryFact(47, "0013", []int{0, 0, 0, 0}, true)},
			cause: func(seqs []int) []int { return []int{seqs[3], seqs[0], seqs[1]} },
			rerun: true,
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h, seqs := zeroCarryHarness(t, 43, tc.facts...)
			a := h.next()
			dead, ok := a.Payload.(*journal.DeadEnd)
			if !ok || dead.Condition != journal.DeadEndFailureAtZero || dead.Core == nil || *dead.Core != 1 || !slices.Equal(a.Cause, tc.cause(seqs)) {
				t.Fatalf("immediate dead end %+v", a)
			}
			want := fmt.Sprintf("core 01 has a failure point at CO 0 carried from session %s; the instability is not caused by Curve Optimizer", zeroCarrySource)
			if tc.rerun {
				want = fmt.Sprintf("core 01 has a failure point at CO 0 carried from session %s; the rerun with every core at CO 0 failed too (#%d), so the instability is not caused by Curve Optimizer", zeroCarrySource, seqs[1])
			}
			if diff := cmp.Diff(want, dead.Detail); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}
