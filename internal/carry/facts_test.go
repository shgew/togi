package carry

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
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
	w.add(&journal.TrialIntent{Trial: id, Core: new(core), Offset: new(-30), Regime: machine.R1, Workload: machine.Workloads(machine.R1)[0].ID, Condition: machine.Alone, Phase: journal.PhaseSearch, DurationS: 90, Profile: profile})
	signal, duration := machine.Signal(""), 90
	if outcome == journal.OutcomeFailure {
		signal, duration = machine.ComputationError, 11
	}
	end := w.add(&journal.TrialEnd{Trial: id, Outcome: outcome, Signal: signal, DurationS: duration})
	failure := 0
	if outcome == journal.OutcomeFailure {
		failure = w.add(&journal.Failure{Trial: id, Core: new(core), Offset: new(-30), Attribution: journal.Attributed, Condition: machine.Alone, Signal: machine.ComputationError})
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
		{name: "new BIOS carries only candidate solo limits", epoch: 1, newBIOS: true},
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
			b.add(&journal.ConfigLoaded{Schema: journal.Schema, Ruleset: 6, Fixes: 1})
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
				carry, err := Prepare(j, journal.Build{Schema: journal.Schema, Ruleset: 7}, entries, &current)
				if err != nil {
					t.Fatal(err)
				}
				if len(carry.Facts) != 0 || len(carry.Cores) != 2 || carry.Cores[0].CandidateSoloLimit == nil || *carry.Cores[0].CandidateSoloLimit != -30 {
					t.Fatalf("new BIOS must keep candidate solo limit without facts: %+v", carry)
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
			b.add(&journal.SessionCarried{Sources: []journal.CarriedSource{src("A", 6)}, FailurePoints: true})
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
				a.add(&journal.ConfigLoaded{Schema: journal.Schema, Ruleset: 6, Fixes: 1})
			}
			aPass, _ := factTrial(a, 0, journal.OutcomePass)
			aFailure, failure := factTrial(a, 1, journal.OutcomeFailure)
			a.add(&journal.TunerDecision{Core: 1, Decision: journal.Backoff}, failure)
			a.add(&journal.ProfileApplied{Offsets: []int{-30, -30}, Condition: machine.Together})
			idle := a.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Condition: machine.Together, Profile: []int{-30, -30}})
			a.add(&journal.TunerDecision{Core: 0, Decision: journal.Backoff}, idle)
			a.archive(dir)
			first, err := prepareFacts(dir, "A", nil, &context, 1)
			if err != nil {
				t.Fatal(err)
			}
			b := newJournal(t, dir, "B", 6, &context, cores...)
			b.add(&journal.ConfigLoaded{Schema: journal.Schema, Ruleset: 6, Fixes: 1})
			for _, f := range first {
				if f.Session == "A" {
					b.add(f.Payload())
				}
			}
			b.add(&journal.SessionCarried{Sources: []journal.CarriedSource{src("A", 6)}, FailurePoints: true})
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
				if c, err := Prepare(j, journal.Build{Schema: journal.Schema, Ruleset: 7}, entries, &context); err == nil || c != nil {
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
	if err := c.ResolveFacts(nil, journal.ConfigBackends{}); err == nil || len(c.Facts) != 0 {
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
	if err := c.ResolveFacts(&context, journal.ConfigBackends{}); err == nil || len(c.Facts) != 0 {
		t.Fatalf("broken deferred source accepted: facts %+v, error %v", c.Facts, err)
	}
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := c.ResolveFacts(&context, journal.ConfigBackends{}); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]factID{key}, factKeys(c.Facts), cmp.AllowUnexported(factID{})); diff != "" {
		t.Fatalf("retry lost original evidence (-want +got):\n%s", diff)
	}
	if err := os.WriteFile(path, []byte("not JSON\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := c.ResolveFacts(&context, journal.ConfigBackends{}); err != nil {
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
		t.Fatalf("corrupt failure point source silently omitted: carry %+v, error %v", c, err)
	}
}

func TestRecordOnlyFactsSurviveTwoTransitionsWithoutFailurePoints(t *testing.T) {
	dir := t.TempDir()
	cores := []machine.CoreInfo{{Core: 0}, {Core: 1}}
	a := newJournal(t, dir, "A", 8, &context, cores...)
	for _, outcome := range []journal.Outcome{journal.OutcomePass, journal.OutcomeFailure} {
		id := string(outcome)
		a.add(&journal.TrialIntent{Trial: id, Cores: []int{0}, Profile: []int{-30, -50}, Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, DurationS: 120, Condition: machine.Together, Phase: journal.PhaseChecking, Cycle: 1, Step: 2, RecordOnly: true})
		signal, duration := machine.Signal(""), 120
		if outcome == journal.OutcomeFailure {
			signal, duration = machine.ComputationError, 11
		}
		end := a.add(&journal.TrialEnd{Trial: id, Outcome: outcome, Signal: signal, DurationS: duration, Core: new(0)})
		if outcome == journal.OutcomeFailure {
			failure := a.add(&journal.Failure{Trial: id, Attribution: journal.Attributed, Core: new(0), Offset: new(-30), Condition: machine.Together, Regime: machine.R7, Signal: signal})
			for i, source := range []int{end, failure} {
				a.add(&journal.HuntStart{Hunt: i + 1, Failure: source, Trial: id, Failing: []int{-30, -50}})
				a.add(&journal.HuntEnd{Hunt: i + 1, Result: "culprit", Cores: []int{0}})
			}
		}
	}
	a.archive(dir)
	first, err := prepareFacts(dir, "A", []defect.Entry{}, &context, 1)
	if err != nil {
		t.Fatal(err)
	}
	if len(first) != 2 || !first[0].RecordOnly || !first[1].RecordOnly {
		t.Fatalf("partial decisive facts were dropped or unmarked: %+v", first)
	}
	derived, err := compute(dir, "A", []defect.Entry{})
	if err != nil {
		t.Fatal(err)
	}
	if len(derived.Cores) != 0 {
		t.Fatalf("live record-only outcomes manufactured failure points: %+v", derived.Cores)
	}
	b := newJournal(t, dir, "B", 9, &context, cores...)
	for _, f := range first {
		payload := f.Payload().(*journal.TrialCarried)
		if !payload.RecordOnly {
			t.Fatalf("carried trial lost record-only marker: %+v", payload)
		}
		seq := b.add(payload)
		if f.Outcome == journal.OutcomeFailure {
			b.add(&journal.HuntStart{Hunt: 1, Failure: seq, Trial: f.Trial, Failing: f.Profile})
			b.add(&journal.HuntEnd{Hunt: 1, Result: "culprit", Cores: []int{0}})
		}
	}
	b.add(&journal.SessionCarried{Sources: derived.Sources, FailurePoints: true, Carried: derived.Cores})
	b.archive(dir)
	second, err := prepareFacts(dir, "B", []defect.Entry{}, &context, 1)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(first, second); diff != "" {
		t.Fatalf("two-hop partial provenance (-want +got):\n%s", diff)
	}
	derived, err = compute(dir, "B", []defect.Entry{})
	if err != nil {
		t.Fatal(err)
	}
	if len(derived.Cores) != 0 {
		t.Fatalf("re-carried record-only failure manufactured failure points: %+v", derived.Cores)
	}
	c := newJournal(t, dir, "C", 10, &context, cores...)
	for _, f := range second {
		c.add(f.Payload())
	}
	c.add(&journal.SessionCarried{Sources: derived.Sources, FailurePoints: true, Carried: derived.Cores})
	c.close()
	copied, err := facts.ReadJournal(filepath.Join(dir, "events.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	if len(copied.Facts) != 0 {
		t.Fatalf("carried facts became new observations: %+v", copied.Facts)
	}
	if diff := cmp.Diff(first, copied.Carried); diff != "" {
		t.Fatalf("second carried extraction (-want +got):\n%s", diff)
	}
}

func TestRulesetSevenToEightRetainsSameBIOSEvidenceAndFailurePoints(t *testing.T) {
	dir := t.TempDir()
	old := newJournal(t, dir, "ruleset-seven", 7, &context, machine.CoreInfo{Core: 0}, machine.CoreInfo{Core: 1})
	pass, _ := factTrial(old, 0, journal.OutcomePass)
	failure, failurePoint := factTrial(old, 1, journal.OutcomeFailure)
	old.close()
	before, err := facts.ReadJournal(filepath.Join(dir, "events.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	j, err := journal.Lock(dir, opts())
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	got, err := Prepare(j, journal.Build{Schema: journal.Schema, Ruleset: 8}, []defect.Entry{}, &context)
	if err != nil {
		t.Fatal(err)
	}
	if got == nil {
		t.Fatal("ruleset-7 to 8 did not prepare a transition")
	}
	if diff := cmp.Diff([]journal.CarriedSource{src("ruleset-seven", 7)}, got.Sources); diff != "" {
		t.Fatalf("transition sources: %s", diff)
	}
	if diff := cmp.Diff([]journal.CarriedCore{
		{Core: 0, CandidateSoloLimit: new(-30), CandidateSoloLimitSession: pass.session, CandidateSoloLimitSeq: pass.seq},
		{Core: 1, FailurePoint: new(-30), FailurePointSession: failure.session, FailurePointSeq: failurePoint, FailurePointSignal: machine.ComputationError},
	}, got.Cores); diff != "" {
		t.Fatalf("ordinary carried values (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff(before.Facts, got.Facts); diff != "" {
		t.Fatalf("ruleset-7 evidence changed (-want +got):\n%s", diff)
	}
	for _, f := range got.Facts {
		if f.RecordOnly {
			t.Fatalf("preexisting trial acquired record-only marker: %+v", f)
		}
	}
	archived, err := facts.ReadJournal(filepath.Join(dir, "archive", "ruleset-seven.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(before.Events, archived.Events); diff != "" {
		t.Fatalf("source journal changed during transition: %s", diff)
	}
}

func TestCarryRequestTelemetryFromRecordedFieldsAndSamples(t *testing.T) {
	for _, mode := range []string{"recorded", "archived samples", "no samples"} {
		t.Run(mode, func(t *testing.T) {
			dir := t.TempDir()
			cores := []machine.CoreInfo{{Core: 0, CCD: 4}, {Core: 1, CCD: 9}}
			a := newJournal(t, dir, "A", 8, &context, cores...)
			a.add(&journal.TrialIntent{Trial: "0001", Regime: machine.R7, Workload: "fixture", Cores: []int{1, 0}, DurationS: 120, Condition: machine.Together, Profile: []int{-30, -40}})
			end := &journal.TrialEnd{Trial: "0001", Outcome: journal.OutcomeFailure, Signal: machine.Stall, StalledCore: new(0), DurationS: 30}
			if mode == "recorded" {
				end.VoltageRequestsV, end.TopRequesters, end.CCDMHz = map[int]float64{0: 1.125, 1: 1.25}, []int{0, 1}, map[int]int{4: 4800, 9: 4900}
			}
			a.add(end)
			a.archive(dir)
			if mode == "archived samples" {
				path := filepath.Join(dir, "archive", "A-trials", "0001")
				if err := os.MkdirAll(path, 0700); err != nil {
					t.Fatal(err)
				}
				sample := "{\"elapsed_ms\":5000,\"pm_table\":{\"voltage_request_v\":[1.125,1.25]},\"core_mhz\":{\"0\":4800,\"1\":4900}}\n"
				if err := os.WriteFile(filepath.Join(path, "samples.jsonl"), []byte(strings.Repeat(sample, 20)), 0600); err != nil {
					t.Fatal(err)
				}
			}
			first, err := prepareFacts(dir, "A", nil, &context, 1)
			if err != nil {
				t.Fatal(err)
			}
			if len(first) != 1 {
				t.Fatalf("carried facts = %d, want 1", len(first))
			}
			got := first[0].Payload().(*journal.TrialCarried)
			var volts map[int]float64
			var top []int
			var mhz map[int]int
			if mode != "no samples" {
				volts, top, mhz = map[int]float64{0: 1.125, 1: 1.25}, []int{0, 1}, map[int]int{4: 4800, 9: 4900}
			}
			if diff := cmp.Diff(volts, got.VoltageRequestsV); diff != "" {
				t.Fatal(diff)
			}
			if diff := cmp.Diff(top, got.TopRequesters); diff != "" {
				t.Fatal(diff)
			}
			if diff := cmp.Diff(mhz, got.CCDMHz); diff != "" {
				t.Fatal(diff)
			}
			if diff := cmp.Diff(new(0), got.StalledCore); diff != "" {
				t.Fatal(diff)
			}
			if mode == "archived samples" {
				got.VoltageRequestsV, got.TopRequesters, got.CCDMHz, got.StalledCore = nil, nil, nil, nil
			}
			b := newJournal(t, dir, "B", 8, &context, cores...)
			b.add(got)
			if mode == "archived samples" {
				b.add(&journal.SessionCarried{Sources: []journal.CarriedSource{src("A", 8)}, FailurePoints: true})
			}
			b.archive(dir)
			resumed, err := prepareFacts(dir, "B", nil, &context, 1)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(first, resumed); diff != "" {
				t.Fatalf("interrupted copy-forward (-want +got):\n%s", diff)
			}
		})
	}
}
