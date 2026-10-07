package carry

import (
	"slices"
	"testing"

	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

// A ruleset-9 hunt left a core-local MCE naming core 1, parked at CO 0, and dead-ended. The ruleset-10 session it
// carries into reruns that failure with every core at CO 0 before any failure_at_zero dead end, unless the failing
// profile was already all at CO 0.
func TestRuleset9FailureAtZeroReachesTheAllZeroRerun(t *testing.T) {
	for _, tc := range []struct {
		name    string
		profile []int
	}{
		{"another core off CO 0", []int{0, 0, -20, -25}},
		{"all at CO 0", []int{0, 0, 0, 0}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			cores := []machine.CoreInfo{{Core: 0, CCD: 0}, {Core: 1, CCD: 0}, {Core: 2, CCD: 1}, {Core: 3, CCD: 1}}
			w := newJournal(t, dir, "X", 9, &context, cores...)
			class := journal.TrialClass{Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Cores: []int{0, 1, 2, 3}, DurationS: 120}
			w.add(&journal.TrialIntent{Trial: "0012", Regime: class.Regime, Workload: class.Workload, Cores: class.Cores, DurationS: class.DurationS, Condition: machine.Parked, Phase: journal.PhaseHunt, Hunt: 1, Group: 1, Profile: tc.profile})
			end := w.add(&journal.TrialEnd{Trial: "0012", Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 40})
			mce := w.add(&journal.MCE{Core: 1, BankType: machine.LoadStore, Trial: "0012"})
			failure := w.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Attributed, Core: new(1), Offset: new(0), Trial: "0012", Regime: class.Regime, Condition: machine.Parked, Profile: tc.profile}, end, mce)
			w.add(&journal.DeadEnd{Condition: journal.DeadEndFailureAtZero, Core: new(1), Detail: "core 01 failed at CO 0"}, failure)
			w.close()
			j, err := journal.Lock(dir, opts())
			if err != nil {
				t.Fatal(err)
			}
			defer j.Close()
			c, err := Prepare(j, journal.Build{Schema: journal.Schema, Ruleset: 10}, []defect.Entry{}, &context)
			if err != nil {
				t.Fatal(err)
			}
			if c == nil || len(c.Cores) != 1 || c.Cores[0].Core != 1 || c.Cores[0].FailurePoint == nil || *c.Cores[0].FailurePoint != 0 || c.Cores[0].FailurePointSeq != failure {
				t.Fatalf("carried failure points %+v", c)
			}

			s := tuner.New()
			var seq int
			add := func(p journal.Payload, cause ...int) int {
				seq++
				s.Fold(journal.Event{Seq: seq, Kind: p.Kind(), Data: p, Cause: cause})
				return seq
			}
			begin := add(&journal.SessionStart{Schema: journal.Schema, Ruleset: 10, Session: "Y", Cores: cores})
			fact := 0
			for _, f := range c.Facts {
				at := add(f.Payload())
				if f.Session == "X" && f.Seq == end {
					fact = at
				}
			}
			if fact == 0 {
				t.Fatalf("the failing trial was not carried: %+v", c.Facts)
			}
			carried := add(&journal.SessionCarried{Sources: c.Sources, FailurePoints: true, Carried: c.Cores})
			phases := make([]int, len(cores))
			for i, core := range cores {
				p := &journal.CorePhase{Core: core.Core, To: journal.PhaseSearch, Reason: "baseline"}
				if core.Core == 1 {
					p.FailurePoint = new(0)
				}
				phases[i] = add(p, begin, carried)
			}

			a := s.Next()
			if !slices.ContainsFunc(tc.profile, func(o int) bool { return o != 0 }) {
				if dead, ok := a.Payload.(*journal.DeadEnd); !ok || dead.Condition != journal.DeadEndFailureAtZero || !slices.Equal(a.Cause, []int{phases[1], fact}) {
					t.Fatalf("an all-zero failing profile must dead-end at once: %+v", a)
				}
				return
			}
			if a.Kind != tuner.RunTrial || !a.Trial.Rerun || a.Trial.Condition != machine.Parked || !slices.Equal(a.Trial.Profile, []int{0, 0, 0, 0}) || a.Trial.Regime != class.Regime || !slices.Equal(a.Cause, []int{fact}) {
				t.Fatalf("first action %+v, want the all-zero rerun of #%d", a, fact)
			}
		})
	}
}
