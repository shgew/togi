package carry

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

var zeroCores = []machine.CoreInfo{{Core: 0, CCD: 0}, {Core: 1, CCD: 0}, {Core: 2, CCD: 1}, {Core: 3, CCD: 1}}

var zeroClass = journal.TrialClass{Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Cores: []int{0, 1, 2, 3}, DurationS: 120}

// zeroFailure records the #343 shape: a parked hunt trial whose core-local MCE named core 1, parked at CO 0. It
// returns the failure's sequence.
func zeroFailure(w *writer, trial string, profile []int) int {
	w.add(&journal.TrialIntent{Trial: trial, Regime: zeroClass.Regime, Workload: zeroClass.Workload, Cores: zeroClass.Cores, DurationS: zeroClass.DurationS, Condition: machine.Parked, Phase: journal.PhaseHunt, Hunt: 1, Group: 1, Profile: profile})
	end := w.add(&journal.TrialEnd{Trial: trial, Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 40})
	mce := w.add(&journal.MCE{Core: 1, BankType: machine.LoadStore, Trial: trial})
	return w.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Attributed, Core: new(1), Offset: new(0), Trial: trial, Regime: zeroClass.Regime, Condition: machine.Parked, Profile: profile}, end, mce)
}

// zeroRerun records the failure's all-zero rerun as ruleset 10 schedules it.
func zeroRerun(w *writer, failure int, outcome journal.Outcome) {
	w.add(&journal.TrialIntent{Trial: "rerun", Regime: zeroClass.Regime, Workload: zeroClass.Workload, Cores: zeroClass.Cores, DurationS: zeroClass.DurationS, Condition: machine.Parked, Phase: journal.PhaseHunt, Rerun: true, Profile: []int{0, 0, 0, 0}}, failure)
	w.add(&journal.TrialEnd{Trial: "rerun", Outcome: outcome, Signal: map[bool]machine.Signal{true: machine.Crash}[outcome == journal.OutcomeFailure], DurationS: 120})
}

// prepareNext prepares the transition from a source session with ruleset to the next ruleset.
func prepareNext(dir string, ruleset int) (*Carry, error) {
	j, err := journal.Lock(dir, opts())
	if err != nil {
		return nil, err
	}
	defer j.Close()
	return Prepare(j, journal.Build{Schema: journal.Schema, Ruleset: ruleset + 1}, []defect.Entry{}, &context)
}

func TestPrepareCarriesOnlyConfirmedFailurePointsAtZero(t *testing.T) {
	for _, tc := range []struct {
		name      string
		ruleset   int
		profile   []int
		rerun     journal.Outcome
		confirmed bool
	}{
		{name: "unconfirmed", ruleset: 9, profile: []int{0, 0, -20, -25}},
		{name: "failing profile all at CO 0", ruleset: 9, profile: []int{0, 0, 0, 0}, confirmed: true},
		{name: "all-zero rerun failed", ruleset: 10, profile: []int{0, 0, -20, -25}, rerun: journal.OutcomeFailure, confirmed: true},
		{name: "all-zero rerun passed", ruleset: 10, profile: []int{0, 0, -20, -25}, rerun: journal.OutcomePass},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			w := newJournal(t, dir, "X", tc.ruleset, &context, zeroCores...)
			failure := zeroFailure(w, "0012", tc.profile)
			if tc.rerun != "" {
				zeroRerun(w, failure, tc.rerun)
			}
			w.close()
			c, err := prepareNext(dir, tc.ruleset)
			if err != nil {
				t.Fatal(err)
			}
			var want []journal.CarriedCore
			if tc.confirmed {
				want = []journal.CarriedCore{{Core: 1, FailurePoint: new(0), FailurePointSession: "X", FailurePointSeq: failure, FailurePointSignal: machine.Crash}}
			}
			if diff := cmp.Diff(want, c.Cores); diff != "" {
				t.Fatalf("carried failure points (-want +got):\n%s", diff)
			}
			if len(c.Facts) == 0 || c.Facts[0].Trial != "0012" || c.Facts[0].Outcome != journal.OutcomeFailure {
				t.Fatalf("the failing trial must still carry as a fact: %+v", c.Facts)
			}
		})
	}
}

// A failure point at CO 0 an older transition carried is checked against the session that recorded the failure.
func TestPrepareChecksCarriedFailurePointsAtZeroAgainstTheirSession(t *testing.T) {
	for _, tc := range []struct {
		name      string
		profile   []int
		archived  bool
		confirmed bool
	}{
		{name: "unconfirmed", profile: []int{0, 0, -20, -25}, archived: true},
		{name: "confirmed", profile: []int{0, 0, 0, 0}, archived: true, confirmed: true},
		{name: "original archive gone", profile: []int{0, 0, 0, 0}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir, original := t.TempDir(), t.TempDir()
			if tc.archived {
				original = dir
			}
			w := newJournal(t, original, "W", 8, &context, zeroCores...)
			failure := zeroFailure(w, "0012", tc.profile)
			w.archive(original)
			x := newJournal(t, dir, "X", 9, &context, zeroCores...)
			x.add(&journal.SessionCarried{Sources: []journal.CarriedSource{src("W", 8)}, FailurePoints: true, Carried: []journal.CarriedCore{{Core: 1, FailurePoint: new(0), FailurePointSession: "W", FailurePointSeq: failure, FailurePointSignal: machine.Crash}}})
			x.close()
			c, err := prepareNext(dir, 9)
			if err != nil {
				t.Fatal(err)
			}
			var want []journal.CarriedCore
			if tc.confirmed {
				want = []journal.CarriedCore{{Core: 1, FailurePoint: new(0), FailurePointSession: "W", FailurePointSeq: failure, FailurePointSignal: machine.Crash}}
			}
			if diff := cmp.Diff(want, c.Cores); diff != "" {
				t.Fatalf("carried failure points (-want +got):\n%s", diff)
			}
		})
	}
}
