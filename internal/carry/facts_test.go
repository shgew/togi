package carry

import (
	"fmt"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func factTrial(w *writer, core int, outcome journal.Outcome) (factID, int) {
	w.t.Helper()
	w.trials++
	id := fmt.Sprintf("%04d", w.trials)
	profile := []int{0, 0}
	profile[core] = -30
	w.add(&journal.TrialIntent{Trial: id, Core: new(core), Offset: new(-30), Regime: machine.R1, Workload: machine.Workloads(machine.R1)[0].ID, Condition: machine.Isolated, Phase: journal.PhaseSearch, DurationS: 90, Profile: profile})
	signal, duration := machine.Signal(""), 90
	if outcome == journal.OutcomeFailure {
		signal, duration = machine.ComputationError, 11
	}
	end := w.add(&journal.TrialEnd{Trial: id, Outcome: outcome, Signal: signal, DurationS: duration})
	failure := 0
	if outcome == journal.OutcomeFailure {
		failure = w.add(&journal.Failure{Trial: id, Core: new(core), Offset: new(-30), Attribution: journal.Attributed, Condition: machine.Isolated, Signal: machine.ComputationError})
	}
	return factID{w.session, end}, failure
}

func factKeys(fs []facts.Fact) []factID {
	var keys []factID
	for _, f := range fs {
		keys = append(keys, factID{f.Session, f.Seq})
	}
	return keys
}

func TestFactEligibilityAcrossArchiveChain(t *testing.T) {
	for _, tc := range []struct {
		name         string
		epoch        int
		biosBoundary bool
		newBIOS      bool
		missingBIOS  bool
		resetAll     bool
		resetCore    bool
	}{
		{name: "reset epochs and defects", epoch: 1},
		{name: "evidence epoch bump", epoch: 2},
		{name: "BIOS archive boundary", epoch: 1, biosBoundary: true},
		{name: "new BIOS carries only edges", epoch: 1, newBIOS: true},
		{name: "unknown current BIOS carries no facts", epoch: 1, missingBIOS: true},
		{name: "newest reset all", epoch: 1, resetAll: true},
		{name: "newest core reset", epoch: 1, resetCore: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			cores := []machine.CoreInfo{{Core: 0}, {Core: 1}}
			a := newJournal(t, dir, "A", 5, &context, cores...)
			factTrial(a, 1, journal.OutcomeFailure)
			a.add(&journal.CommandReset{All: true})
			aFailure, _ := factTrial(a, 1, journal.OutcomeFailure)
			factTrial(a, 1, journal.OutcomePass)
			a.archive(dir)
			bContext := context
			if tc.biosBoundary {
				bContext.BIOSVersion = "other"
			}
			b := newJournal(t, dir, "B", 6, &bContext, cores...)
			factTrial(b, 0, journal.OutcomeFailure)
			b.add(&journal.CommandReset{Core: new(0)})
			bFailure, _ := factTrial(b, 0, journal.OutcomeFailure)
			bPass, _ := factTrial(b, 1, journal.OutcomePass)
			_, excluded := factTrial(b, 1, journal.OutcomeFailure)
			b.add(&journal.TunerDecision{Core: 1, Decision: journal.Backoff}, excluded)
			b.add(&journal.ConfigLoaded{Schema: 2, Ruleset: 6, Fixes: 1})
			bFixed, fixed := factTrial(b, 1, journal.OutcomeFailure)
			b.add(&journal.TunerDecision{Core: 1, Decision: journal.Backoff}, fixed)
			b.archive(dir)
			c := newJournal(t, dir, "C", 6, &context, cores...)
			if tc.resetAll {
				c.add(&journal.CommandReset{All: true})
			}
			if tc.resetCore {
				c.add(&journal.CommandReset{Core: new(1)})
			}
			cPass, _ := factTrial(c, 0, journal.OutcomePass)
			cFailure, _ := factTrial(c, 1, journal.OutcomeFailure)
			c.archive(dir)
			entries := []defect.Entry{{ID: 1, Decisions: []defect.DecisionMatch{{Kind: journal.KindTunerDecision, Decision: journal.Backoff, Cause: journal.KindFailure}}}}
			current := context
			if tc.newBIOS {
				current.BIOSVersion = "new"
			}
			currentContext := &current
			if tc.missingBIOS {
				currentContext = nil
			}
			got, err := prepareFacts(dir, "C", entries, currentContext, tc.epoch)
			if err != nil {
				t.Fatal(err)
			}
			var want []factID
			if !tc.newBIOS && !tc.missingBIOS {
				if !tc.biosBoundary && !tc.resetAll {
					if !tc.resetCore {
						want = append(want, aFailure)
					}
					want = append(want, bFailure)
					if !tc.resetCore {
						if tc.epoch == 1 {
							want = append(want, bPass)
						}
						want = append(want, bFixed)
					}
				}
				if tc.epoch == 1 {
					want = append(want, cPass)
				}
				want = append(want, cFailure)
			}
			if diff := cmp.Diff(want, factKeys(got), cmp.AllowUnexported(factID{})); diff != "" {
				t.Fatalf("eligible original identities (-want +got):\n%s", diff)
			}
			if tc.newBIOS {
				if err := os.WriteFile(filepath.Join(dir, "archive", "C-carry-pending"), nil, 0o644); err != nil {
					t.Fatal(err)
				}
				j, err := journal.Lock(dir, opts())
				if err != nil {
					t.Fatal(err)
				}
				defer j.Close()
				carry, err := Prepare(j, journal.Build{Schema: 2, Ruleset: 7}, entries, &current)
				if err != nil {
					t.Fatal(err)
				}
				if len(carry.Facts) != 0 || len(carry.Cores) != 2 || carry.Cores[0].Edge == nil || *carry.Cores[0].Edge != -30 {
					t.Fatalf("new BIOS must keep candidate edge without facts: %+v", carry)
				}
			}
		})
	}
}

func TestFactCopyForwardPreservesProvenanceAndStopsWalk(t *testing.T) {
	for _, epoch := range []int{1, 2} {
		t.Run(fmt.Sprintf("epoch %d", epoch), func(t *testing.T) {
			dir := t.TempDir()
			cores := []machine.CoreInfo{{Core: 0}, {Core: 1}}
			a := newJournal(t, dir, "A", 6, &context, cores...)
			aPass, _ := factTrial(a, 0, journal.OutcomePass)
			aFailure, _ := factTrial(a, 1, journal.OutcomeFailure)
			a.archive(dir)
			first, err := prepareFacts(dir, "A", nil, &context, 1)
			if err != nil {
				t.Fatal(err)
			}
			b := newJournal(t, dir, "B", 6, &context, cores...)
			for _, f := range first {
				b.add(f.Payload())
			}
			b.add(&journal.SessionCarried{Sources: []journal.CarriedSource{src("A", 6)}, Marks: true})
			bOwn, _ := factTrial(b, 0, journal.OutcomeFailure)
			b.archive(dir)
			// A missing original archive retains copied facts without restarting collection.
			if err := os.Remove(filepath.Join(dir, "archive", "A.jsonl")); err != nil {
				t.Fatal(err)
			}
			second, err := prepareFacts(dir, "B", nil, &context, epoch)
			if err != nil {
				t.Fatal(err)
			}
			want := []factID{aFailure, bOwn}
			if epoch == 1 {
				want = append([]factID{aPass}, want...)
			}
			if diff := cmp.Diff(want, factKeys(second), cmp.AllowUnexported(factID{})); diff != "" {
				t.Fatalf("second transition (-want +got):\n%s", diff)
			}
			for _, f := range second {
				if f.Session == "A" {
					for _, original := range first {
						if original.Seq == f.Seq {
							if diff := cmp.Diff(original, f); diff != "" {
								t.Fatalf("original provenance changed (-want +got):\n%s", diff)
							}
						}
					}
				}
			}
		})
	}
}

func TestCopiedFailuresRecheckedAgainstOriginalDefects(t *testing.T) {
	for _, tc := range []struct {
		name        string
		fixed       bool
		missing     bool
		broken      bool
		wantFailure bool
	}{
		{name: "new defect drops original trial and idle failures"},
		{name: "original fixes stamp protects failures", fixed: true, wantFailure: true},
		{name: "missing original archive retains failures", missing: true, wantFailure: true},
		{name: "unreadable original archive stops carry", broken: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			cores := []machine.CoreInfo{{Core: 0}, {Core: 1}}
			older := newJournal(t, dir, "0", 6, &context, cores...)
			factTrial(older, 0, journal.OutcomeFailure)
			older.archive(dir)
			a := newJournal(t, dir, "A", 6, &context, cores...)
			if tc.fixed {
				a.add(&journal.ConfigLoaded{Schema: 2, Ruleset: 6, Fixes: 1})
			}
			aPass, _ := factTrial(a, 0, journal.OutcomePass)
			aFailure, failure := factTrial(a, 1, journal.OutcomeFailure)
			a.add(&journal.TunerDecision{Core: 1, Decision: journal.Backoff}, failure)
			a.add(&journal.ProfileApplied{Offsets: []int{-30, -30}, Condition: machine.Resident})
			idle := a.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Condition: machine.Resident, Profile: []int{-30, -30}})
			a.add(&journal.TunerDecision{Core: 0, Decision: journal.Backoff}, idle)
			a.archive(dir)
			first, err := prepareFacts(dir, "A", nil, &context, 1)
			if err != nil {
				t.Fatal(err)
			}
			b := newJournal(t, dir, "B", 6, &context, cores...)
			b.add(&journal.ConfigLoaded{Schema: 2, Ruleset: 6, Fixes: 1})
			for _, f := range first {
				if f.Session == "A" {
					b.add(f.Payload())
				}
			}
			b.add(&journal.SessionCarried{Sources: []journal.CarriedSource{src("A", 6)}, Marks: true})
			bOwn, _ := factTrial(b, 0, journal.OutcomeFailure)
			b.archive(dir)
			original := filepath.Join(dir, "archive", "A.jsonl")
			if tc.missing {
				if err := os.Remove(original); err != nil {
					t.Fatal(err)
				}
			}
			if tc.broken {
				if err := os.WriteFile(original, []byte("{not JSON}\n"), 0o644); err != nil {
					t.Fatal(err)
				}
			}
			entries := []defect.Entry{{ID: 1, Decisions: []defect.DecisionMatch{{Kind: journal.KindTunerDecision, Decision: journal.Backoff, Cause: journal.KindFailure}}}}
			second, err := prepareFacts(dir, "B", entries, &context, 1)
			if tc.broken {
				if err == nil {
					t.Fatal("unreadable original journal must stop defect rechecking")
				}
				if err := os.WriteFile(filepath.Join(dir, "archive", "B-carry-pending"), nil, 0o600); err != nil {
					t.Fatal(err)
				}
				j, err := journal.Lock(dir, opts())
				if err != nil {
					t.Fatal(err)
				}
				defer j.Close()
				if c, err := Prepare(j, journal.Build{Schema: 2, Ruleset: 7}, entries, &context); err == nil || c != nil {
					t.Fatalf("Prepare committed carry despite unreadable original defect evidence: carry %+v, error %v", c, err)
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			want := []factID{aPass}
			if tc.wantFailure {
				want = append(want, aFailure, factID{"A", idle})
			}
			want = append(want, bOwn)
			if diff := cmp.Diff(want, factKeys(second), cmp.AllowUnexported(factID{})); diff != "" {
				t.Fatalf("second transition must recheck original decisions without collecting older facts (-want +got):\n%s", diff)
			}
		})
	}
}

func TestDeferredFactsRequireContextAndRetryFailedRead(t *testing.T) {
	dir := t.TempDir()
	w := newJournal(t, dir, "A", 3, &context, machine.CoreInfo{Core: 0}, machine.CoreInfo{Core: 1})
	key, _ := factTrial(w, 0, journal.OutcomeFailure)
	w.close()
	c := prepare(t, dir, []defect.Entry{})
	if err := c.ResolveFacts(nil); err == nil || len(c.Facts) != 0 {
		t.Fatalf("unknown BIOS context accepted: facts %+v, error %v", c.Facts, err)
	}
	path := filepath.Join(dir, "archive", "A.jsonl")
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte("not JSON\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := c.ResolveFacts(&context); err == nil || len(c.Facts) != 0 {
		t.Fatalf("broken deferred source accepted: facts %+v, error %v", c.Facts, err)
	}
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := c.ResolveFacts(&context); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]factID{key}, factKeys(c.Facts), cmp.AllowUnexported(factID{})); diff != "" {
		t.Fatalf("retry lost original evidence (-want +got):\n%s", diff)
	}
	if err := os.WriteFile(path, []byte("not JSON\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := c.ResolveFacts(&context); err != nil {
		t.Fatalf("resolved carry reread its source: %v", err)
	}
	if diff := cmp.Diff([]factID{key}, factKeys(c.Facts), cmp.AllowUnexported(factID{})); diff != "" {
		t.Fatalf("resolved evidence changed (-want +got):\n%s", diff)
	}
}

func TestFactWalkRejectsCorruptOlderArchive(t *testing.T) {
	dir := t.TempDir()
	a := newJournal(t, dir, "A", 2, &context)
	a.archive(dir)
	b := newJournal(t, dir, "B", 3, &context)
	b.archive(dir)
	if err := os.WriteFile(filepath.Join(dir, "archive", "A.jsonl"), []byte("not JSON\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	got, err := prepareFacts(dir, "B", nil, &context, 1)
	if err == nil || got != nil {
		t.Fatalf("corrupt archive silently omitted: facts %+v, error %v", got, err)
	}
	if c, err := compute(dir, "B", nil); err == nil || c != nil {
		t.Fatalf("corrupt mark source silently omitted: carry %+v, error %v", c, err)
	}
}
