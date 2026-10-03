package facts

import (
	"path/filepath"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/tuner"
)

func TestCarriedFactsKeepOriginalProvenance(t *testing.T) {
	build := journal.Build{Version: "0.7.0", Rev: "original", Schema: 2, Ruleset: 6, Fixes: 3}
	idle := &journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Core: new(3), Offset: new(-30), Regime: machine.R7, Condition: machine.Parked, Profile: []int{-20, -30, -40}}
	original := FromEvents([]journal.Event{
		{Seq: 1, Data: &journal.SessionStart{Build: build, Session: "original", Evidence: 4, Cores: []machine.CoreInfo{{Core: 7}, {Core: 1}, {Core: 3}}}},
		{Seq: 2, Boot: "intent-boot", Data: &journal.TrialIntent{Trial: "0304", Regime: machine.R7, Workload: "workload", Cores: []int{7, 3}, DurationS: 120, Condition: machine.Together, Phase: journal.PhaseChecking, Rerun: true, RecordOnly: true, Profile: []int{-22, -30, -50}}},
		{Seq: 3, Data: &journal.ConfigLoaded{Version: "later", Ruleset: 6}},
		{Seq: 4, Time: time.Unix(40, 0).UTC(), Boot: "end-boot", Data: &journal.TrialEnd{Trial: "0304", Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(7), DurationS: 11}},
		{Seq: 5, Time: time.Unix(50, 0).UTC(), Boot: "idle-boot", Data: idle},
	})
	if diff := cmp.Diff([]int{3, 7}, original.Facts[0].Class.Cores); diff != "" {
		t.Fatal(diff)
	}
	if original.Facts[0].Build != build || original.Facts[0].Boot != "intent-boot" || original.Facts[0].Epoch != 4 || original.Facts[0].Class.DurationS != 120 || original.Facts[0].DurationS != 11 {
		t.Fatalf("original trial provenance: %+v", original.Facts[0])
	}
	if !original.Facts[0].RecordOnly || original.Facts[1].RecordOnly {
		t.Fatalf("record-only marker changed across extraction: %+v", original.Facts)
	}
	if diff := cmp.Diff(idle, original.Facts[1].Idle); diff != "" {
		t.Fatal(diff)
	}
	for _, id := range []string{"first-copy", "second-copy"} {
		path := filepath.Join(t.TempDir(), "events.jsonl")
		events := []journal.Event{{Data: &journal.SessionStart{Schema: 2, Ruleset: 6, Session: id, Evidence: 9, Cores: original.Cores}}}
		copies := original.Facts
		if original.Carried != nil {
			copies = original.Carried
		}
		for _, f := range copies {
			events = append(events, journal.Event{Time: time.Unix(100, 0).UTC(), Boot: "copy-boot", Data: f.Payload()})
		}
		events = append(events,
			journal.Event{Data: &journal.TrialIntent{Trial: "own", Core: new(1), Offset: new(-10), Profile: []int{-10, 0, 0}, Condition: machine.Alone}},
			journal.Event{Data: &journal.TrialEnd{Trial: "own", Outcome: journal.OutcomePass}},
		)
		writeJournal(t, path, events, "")
		copied, err := ReadJournal(path)
		if err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff(copies, copied.Carried); diff != "" {
			t.Fatalf("%s changed provenance: %s", id, diff)
		}
		if copied.Epoch != 9 || len(copied.Facts) != 1 || copied.Facts[0].Session != id || copied.Facts[0].Trial != "own" || copied.Facts[0].Epoch != 9 || copied.Facts[0].RecordOnly {
			t.Fatalf("own facts mixed with carried: %+v", copied)
		}
		original = copied
	}
}

func TestCarriedEventsDoNotChangeTuning(t *testing.T) {
	before, after := tuner.New(), tuner.New()
	var beforeProjection, afterProjection journal.State
	for _, e := range []journal.Event{
		{Seq: 1, Data: &journal.SessionStart{Schema: 2, Session: "current", Cores: []machine.CoreInfo{{Core: 0}, {Core: 1}}}},
		{Seq: 2, Data: &journal.CorePhase{Core: 0, To: journal.PhaseSearch, Offset: -10}},
		{Seq: 3, Data: &journal.CorePhase{Core: 1, To: journal.PhaseSearch, Offset: -20}},
	} {
		before.Fold(e)
		after.Fold(e)
		beforeProjection.Fold(e)
		afterProjection.Fold(e)
	}
	for i, payload := range []journal.Payload{
		&journal.TrialCarried{Source: journal.FactSource{Session: "old", Seq: 40, Trial: "0040", Evidence: 1}, Class: journal.TrialClass{Regime: machine.R1, Cores: []int{0}, DurationS: 90}, Condition: machine.Alone, Profile: []int{-50, 0}, Outcome: journal.OutcomePass, DurationS: 90},
		&journal.FailureCarried{Source: journal.FactSource{Session: "old", Seq: 50, Evidence: 1}, Class: journal.TrialClass{Regime: machine.R6, Cores: []int{0, 1}}, Attribution: journal.Unattributed, Signal: machine.Crash, Condition: machine.Together, Profile: []int{-50, -50}},
	} {
		e := journal.Event{Seq: i + 4, Data: payload, Kind: payload.Kind()}
		after.Fold(e)
		afterProjection.Fold(e)
	}
	before.Project(&beforeProjection)
	after.Project(&afterProjection)
	beforeProjection.LastSeq = afterProjection.LastSeq
	if diff := journal.DiffFields(beforeProjection, afterProjection); diff != nil {
		t.Fatalf("carried events changed projection: %v", diff)
	}
	if diff := cmp.Diff(before.Next(), after.Next()); diff != "" {
		t.Fatalf("carried events changed decision: %s", diff)
	}
}

func TestCarryReaderReconstructsHistoricalProfiles(t *testing.T) {
	path := filepath.Join(t.TempDir(), "events.jsonl")
	writeJournal(t, path, []journal.Event{
		{Data: &journal.SessionStart{Schema: 1, Ruleset: 6, Session: "historical", Cores: []machine.CoreInfo{{Core: 7}, {Core: 1}, {Core: 3}}}},
		{Data: &journal.ProfileApplied{Offsets: []int{-10, -20, -30}, Condition: machine.Together}},
		{Data: &journal.TrialIntent{Trial: "applied", Cores: []int{7, 1}, Regime: machine.R7, DurationS: 120, Condition: machine.Together}},
		{Data: &journal.TrialEnd{Trial: "applied", Outcome: journal.OutcomePass, DurationS: 120}},
		{Data: &journal.ProfileChange{To: []int{-11, -22, -33}}},
		{Data: &journal.SMUReadback{Core: 3, Offset: -25}},
		{Data: &journal.TrialIntent{Trial: "readback", Cores: []int{7, 3}, Regime: machine.R7, DurationS: 900, Condition: machine.Parked}},
		{Data: &journal.TrialEnd{Trial: "readback", Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 11}},
	}, "")
	events, err := journal.ReadForCarry(path)
	if err != nil {
		t.Fatal(err)
	}
	s := FromEvents(events)
	want := [][]int{{-10, -20, -30}, {-11, -25, -33}}
	for i, fact := range s.Facts {
		if diff := cmp.Diff(want[i], fact.Profile); diff != "" {
			t.Fatal(diff)
		}
		if fact.Epoch != 1 {
			t.Fatalf("historical fact epoch = %d, want 1", fact.Epoch)
		}
	}
	if len(s.Facts) != len(want) {
		t.Fatalf("decisive facts = %d, want %d", len(s.Facts), len(want))
	}
}

func TestRecordOnlyFactsRetainTrialClassAndOutcomes(t *testing.T) {
	for _, outcome := range []journal.Outcome{journal.OutcomePass, journal.OutcomeFailure, journal.OutcomeInconclusive} {
		t.Run(string(outcome), func(t *testing.T) {
			intent := &journal.TrialIntent{Trial: "partial", Cores: []int{7, 3}, Regime: machine.R7, Workload: "workload", DurationS: 120, Condition: machine.Together, Phase: journal.PhaseChecking, Lap: 2, Step: 4, RecordOnly: true, Profile: []int{-20, -30, -50}}
			ordinary := *intent
			ordinary.RecordOnly, ordinary.Step = false, 0
			if diff := cmp.Diff(ClassOf(&ordinary), ClassOf(intent)); diff != "" {
				t.Fatalf("marker changed trial class: %s", diff)
			}
			path := filepath.Join(t.TempDir(), "events.jsonl")
			writeJournal(t, path, []journal.Event{
				{Data: &journal.SessionStart{Schema: journal.Schema, Ruleset: 8, Session: "original", Cores: []machine.CoreInfo{{Core: 1}, {Core: 3}, {Core: 7}}}},
				{Boot: "intent-boot", Data: intent},
				{Data: &journal.TrialEnd{Trial: "partial", Outcome: outcome, Signal: machine.ComputationError, Core: new(7), DurationS: 11}},
			}, "")
			session, err := ReadJournal(path)
			if err != nil {
				t.Fatal(err)
			}
			if !session.Trials[0].Intent.RecordOnly || session.Trials[0].Intent.Step != 4 {
				t.Fatalf("partial intent fields lost: %+v", session.Trials[0].Intent)
			}
			if outcome == journal.OutcomeInconclusive {
				if len(session.Facts) != 0 {
					t.Fatalf("inconclusive manufactured facts: %+v", session.Facts)
				}
				return
			}
			if len(session.Facts) != 1 || !session.Facts[0].RecordOnly || session.Facts[0].Outcome != outcome || session.Facts[0].Core == nil || *session.Facts[0].Core != 7 {
				t.Fatalf("record-only decisive outcome lost: %+v", session.Facts)
			}
		})
	}
}
