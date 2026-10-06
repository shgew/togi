package carry

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"

	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestPrepareReadsArchivesOfPatternLikeStateDir(t *testing.T) {
	carryFrom := func(t *testing.T, dir string) (*Carry, []factID) {
		t.Helper()
		if err := os.Mkdir(dir, 0o755); err != nil {
			t.Fatal(err)
		}
		cores := []machine.CoreInfo{{Core: 0}, {Core: 1}}
		a := newJournal(t, dir, "A", 6, &context, cores...)
		factTrial(a, 0, journal.OutcomePass)
		factTrial(a, 1, journal.OutcomeFailure)
		a.archive(dir)
		newJournal(t, dir, "X", 7, &context, cores...).close()
		j, err := journal.Lock(dir, opts())
		if err != nil {
			t.Fatal(err)
		}
		defer j.Close()
		c, err := Prepare(j, journal.Build{Schema: journal.Schema, Ruleset: 8}, []defect.Entry{}, &context)
		if err != nil {
			t.Fatal(err)
		}
		return c, factKeys(c.Facts)
	}
	want, wantFacts := carryFrom(t, filepath.Join(t.TempDir(), "state"))
	if diff := cmp.Diff([]journal.CarriedSource{src("X", 7), src("A", 6)}, want.Sources); diff != "" {
		t.Fatalf("ordinary state directory sources (-want +got):\n%s", diff)
	}
	if len(want.Cores) != 2 || len(wantFacts) != 2 {
		t.Fatalf("ordinary state directory carried %d cores and %d facts, want 2 and 2", len(want.Cores), len(wantFacts))
	}
	for _, name := range []string{"state[", "state[1]", "state*", "state?"} {
		t.Run(name, func(t *testing.T) {
			got, gotFacts := carryFrom(t, filepath.Join(t.TempDir(), name))
			if diff := cmp.Diff(want, got, cmpopts.IgnoreUnexported(Carry{}), cmpopts.IgnoreFields(Carry{}, "Facts")); diff != "" {
				t.Errorf("carry (-ordinary +got):\n%s", diff)
			}
			if diff := cmp.Diff(wantFacts, gotFacts, cmp.AllowUnexported(factID{})); diff != "" {
				t.Errorf("carried facts (-ordinary +got):\n%s", diff)
			}
		})
	}
}
