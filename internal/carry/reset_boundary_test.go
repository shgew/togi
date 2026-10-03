package carry

import (
	"fmt"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestFactPrefixNeedsCarryCommitmentToStopTraversal(t *testing.T) {
	for _, committed := range []bool{false, true} {
		t.Run(fmt.Sprintf("committed %v", committed), func(t *testing.T) {
			dir := t.TempDir()
			cores := []machine.CoreInfo{{Core: 0}, {Core: 1}}
			a := newJournal(t, dir, "A", 6, &context, cores...)
			aPass, _ := factTrial(a, 0, journal.OutcomePass)
			aFailure, _ := factTrial(a, 1, journal.OutcomeFailure)
			a.archive(dir)
			originals, err := prepareFacts(dir, "A", nil, &context, 1)
			if err != nil {
				t.Fatal(err)
			}
			b := newJournal(t, dir, "B", 6, &context, cores...)
			b.add(originals[0].Payload())
			if committed {
				b.add(&journal.SessionCarried{Sources: []journal.CarriedSource{src("A", 6)}, FailurePoints: true})
			}
			own, _ := factTrial(b, 0, journal.OutcomeFailure)
			b.archive(dir)
			got, err := prepareFacts(dir, "B", nil, &context, 1)
			if err != nil {
				t.Fatal(err)
			}
			want := []factID{aPass, own}
			if !committed {
				want = []factID{aPass, aFailure, own}
			}
			if diff := cmp.Diff(want, factKeys(got), cmp.AllowUnexported(factID{})); diff != "" {
				t.Fatalf("prefix traversal and original-identity dedup (-want +got):\n%s", diff)
			}
		})
	}
}

func TestFactResetBoundaryExcludesDiscardedSourcesAndCopies(t *testing.T) {
	dir := t.TempDir()
	cores := []machine.CoreInfo{{Core: 0}, {Core: 1}}
	a := newJournal(t, dir, "A", 6, &context, cores...)
	factTrial(a, 0, journal.OutcomePass)
	factTrial(a, 1, journal.OutcomeFailure)
	a.archive(dir)
	old, err := prepareFacts(dir, "A", nil, &context, 1)
	if err != nil {
		t.Fatal(err)
	}
	j, err := journal.Lock(dir, opts())
	if err != nil {
		t.Fatal(err)
	}
	if err := j.MarkResetAll(); err != nil {
		t.Fatal(err)
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	// A reset boundary must prevent even attempting to read discarded archives.
	if err := os.WriteFile(filepath.Join(dir, "archive", "A.jsonl"), []byte("not JSON\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	b := newJournal(t, dir, "B", 6, &context, cores...)
	for _, f := range old {
		b.add(f.Payload())
	}
	b.add(&journal.SessionCarried{Sources: []journal.CarriedSource{src("A", 6)}, FailurePoints: true})
	own, _ := factTrial(b, 0, journal.OutcomePass)
	b.archive(dir)
	got, err := prepareFacts(dir, "B", nil, &context, 1)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]factID{own}, factKeys(got), cmp.AllowUnexported(factID{})); diff != "" {
		t.Fatalf("reset boundary must keep only post-reset facts (-want +got):\n%s", diff)
	}
	got, err = prepareFacts(dir, "A", nil, &context, 1)
	if err != nil || len(got) != 0 {
		t.Fatalf("discarded source must not be read: facts %+v, error %v", got, err)
	}
}

func TestResetBoundaryFiltersCopiedCandidateValues(t *testing.T) {
	dir := t.TempDir()
	a := newJournal(t, dir, "A", 5, &context)
	a.archive(dir)
	j, err := journal.Lock(dir, opts())
	if err != nil {
		t.Fatal(err)
	}
	if err := j.MarkResetAll(); err != nil {
		t.Fatal(err)
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	b := newJournal(t, dir, "B", 6, &context)
	b.add(&journal.SessionCarried{
		Sources:       []journal.CarriedSource{src("A", 5)},
		FailurePoints: true,
		Carried: []journal.CarriedCore{
			{Core: 0, CandidateSoloLimit: new(-30), CandidateSoloLimitSession: "A", CandidateSoloLimitSeq: 3},
			{Core: 1, FailurePoint: new(-5), FailurePointSession: "A", FailurePointSeq: 4, FailurePointSignal: machine.ComputationError},
		},
	})
	b.close()
	j, err = journal.Lock(dir, opts())
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	c, err := Prepare(j, journal.Build{Schema: journal.Schema, Ruleset: 7}, nil, &context)
	if err != nil {
		t.Fatal(err)
	}
	if c == nil || len(c.Cores) != 0 {
		t.Fatalf("reset revived copied candidate values: %+v", c)
	}
}

func TestResetBoundaryFinishesInterruptedPendingDrop(t *testing.T) {
	dir := t.TempDir()
	a := newJournal(t, dir, "A", 5, &context)
	a.archive(dir)
	marker := filepath.Join(dir, "archive", "A-carry-pending")
	if err := os.WriteFile(marker, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	j, err := journal.Lock(dir, opts())
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	if err := j.MarkResetAll(); err != nil {
		t.Fatal(err)
	}
	// Crash after the boundary, before the pending source was removed.
	if err := os.WriteFile(filepath.Join(dir, "archive", "A.jsonl"), []byte("not JSON\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	c, err := Prepare(j, journal.Build{Schema: journal.Schema, Ruleset: 7}, nil, &context)
	if err != nil || c != nil {
		t.Fatalf("reset source survived interrupted marker removal: carry %+v, error %v", c, err)
	}
	if _, err := os.Stat(marker); !os.IsNotExist(err) {
		t.Fatalf("discarded pending source still present: %v", err)
	}
}
