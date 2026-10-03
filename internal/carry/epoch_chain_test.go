package carry

import (
	"fmt"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestInterruptedEpochTransitionRetainsOriginalSoloLimitsAndFailurePoints(t *testing.T) {
	for _, evidence := range []int{0, 1} {
		t.Run(fmt.Sprintf("source-evidence-%d", evidence), func(t *testing.T) {
			dir := t.TempDir()
			old, err := journal.Open(dir, opts())
			if err != nil {
				t.Fatal(err)
			}
			a := &writer{t: t, j: old, session: "20261001T000000Z"}
			cores := []machine.CoreInfo{{Core: 0}, {Core: 1}}
			a.add(&journal.SessionStart{Schema: journal.Schema, Ruleset: 6, Evidence: evidence, Session: a.session, Cores: cores})
			a.add(&journal.SessionContext{BIOSContext: context})
			factTrial(a, 0, journal.OutcomePass)
			failure, _ := factTrial(a, 1, journal.OutcomeFailure)
			a.close()

			j, err := journal.Lock(dir, opts())
			if err != nil {
				t.Fatal(err)
			}
			defer j.Close()
			build := journal.Build{Schema: journal.Schema, Ruleset: 6, EvidenceEpoch: 2}
			first, err := Prepare(j, build, []defect.Entry{}, &context)
			if err != nil {
				t.Fatal(err)
			}
			if first == nil || len(first.Cores) != 2 || first.Cores[0].SoloLimit == nil || *first.Cores[0].SoloLimit != -30 || first.Cores[1].FailurePoint == nil || *first.Cores[1].FailurePoint != -30 {
				t.Fatalf("first epoch transition did not preserve source evidence: %+v", first)
			}
			if len(first.Facts) != 1 || first.Facts[0].Outcome != journal.OutcomeFailure || first.Facts[0].Session != failure.session || first.Facts[0].Seq != failure.seq {
				t.Fatalf("first transition must retain the original failure, without old-epoch passes: %+v", first.Facts)
			}
			if err := j.Open(); err != nil {
				t.Fatal(err)
			}
			p := &writer{t: t, j: j, session: "20261002T000000Z"}
			p.add(&journal.SessionStart{Build: build, Evidence: build.Epoch(), Session: p.session, Cores: cores})
			p.add(&journal.SessionContext{BIOSContext: context})
			// Interrupt after context, before facts and the session.carried commitment.
			p.close()

			j, err = journal.Lock(dir, opts())
			if err != nil {
				t.Fatal(err)
			}
			defer j.Close()
			build.EvidenceEpoch = 3
			second, err := Prepare(j, build, []defect.Entry{}, &context)
			if err != nil {
				t.Fatal(err)
			}
			if second == nil {
				t.Fatal("second epoch upgrade did not prepare carry")
			}
			if diff := cmp.Diff(first.Cores, second.Cores); diff != "" {
				t.Fatalf("interrupted same-ruleset epoch chain lost original solo limits/failure points (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff(first.Facts, second.Facts); diff != "" {
				t.Fatalf("interrupted epoch chain changed original failure facts (-want +got):\n%s", diff)
			}
			if len(second.Sources) != 2 || second.Sources[0].Session != p.session || second.Sources[1].Session != a.session {
				t.Fatalf("source chain must retain incomplete epoch two and original epoch one: %+v", second.Sources)
			}
		})
	}
}
