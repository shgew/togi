package carry

import (
	"fmt"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestResetOfLoadedCoreFiltersCopiedFacts(t *testing.T) {
	for _, all := range []bool{false, true} {
		t.Run(fmt.Sprintf("reset all %v", all), func(t *testing.T) {
			dir := t.TempDir()
			cores := []machine.CoreInfo{{Core: 0}, {Core: 1}}
			a := newJournal(t, dir, "A", 6, &context, cores...)
			factTrial(a, 0, journal.OutcomePass)
			aCore1, _ := factTrial(a, 1, journal.OutcomeFailure)
			a.add(&journal.Failure{Attribution: journal.Unattributed, Condition: machine.Together, Profile: []int{-30, -30}, Signal: machine.Crash}, 2)
			a.archive(dir)
			copied, err := prepareFacts(dir, "A", nil, &context, 1)
			if err != nil {
				t.Fatal(err)
			}
			b := newJournal(t, dir, "B", 6, &context, cores...)
			for _, f := range copied {
				b.add(f.Payload())
			}
			reset := &journal.CommandReset{Core: new(0)}
			if all {
				reset = &journal.CommandReset{All: true}
			}
			b.add(reset)
			own, _ := factTrial(b, 0, journal.OutcomePass)
			b.archive(dir)
			got, err := prepareFacts(dir, "B", nil, &context, 1)
			if err != nil {
				t.Fatal(err)
			}
			want := []factID{own}
			if !all {
				want = []factID{aCore1, own}
			}
			if diff := cmp.Diff(want, factKeys(got), cmp.AllowUnexported(factID{})); diff != "" {
				t.Fatalf("facts after reset (-want +got):\n%s", diff)
			}
		})
	}
}
