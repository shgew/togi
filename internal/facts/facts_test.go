package facts

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func writeJournal(t *testing.T, path string, events []journal.Event, tail string) {
	t.Helper()
	var lines strings.Builder
	for i, e := range events {
		e.Seq = i + 1
		if e.Data != nil {
			e.Kind, e.Msg = e.Data.Kind(), e.Data.Message()
		}
		data, err := json.Marshal(e)
		if err != nil {
			t.Fatal(err)
		}
		lines.Write(data)
		lines.WriteByte('\n')
	}
	lines.WriteString(tail)
	if err := os.WriteFile(path, []byte(lines.String()), 0o600); err != nil {
		t.Fatal(err)
	}
}

func TestReadJournalDecisiveFacts(t *testing.T) {
	for _, schema := range []int{1, 2} {
		t.Run(fmt.Sprint(schema), func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "events.jsonl")
			build := journal.Build{Version: "old", Rev: "aaaa", Schema: schema, Ruleset: 1}
			context := machine.BIOSContext{BIOSVersion: "1.2", Board: "board", CPUModel: "cpu"}
			var events []journal.Event
			add := func(p journal.Payload) {
				events = append(events, journal.Event{Time: time.Unix(int64(len(events)), 0).UTC(), Boot: "a", Data: p})
			}
			add(&journal.SessionStart{Build: build, Session: "20261001T000000Z", Cores: []machine.CoreInfo{{Core: 4}, {Core: 2}}})
			add(&journal.SessionContext{BIOSContext: context})
			add(&journal.ProfileApplied{Offsets: []int{-20, -30}, Condition: machine.Together})
			add(&journal.TrialIntent{Trial: "alone", Core: new(4), Offset: new(-10), Regime: machine.R1, Workload: "one", DurationS: 90, Condition: machine.Alone, Phase: journal.PhaseSearch})
			add(&journal.TrialEnd{Trial: "alone", Outcome: journal.OutcomePass, DurationS: 89})
			add(&journal.TrialIntent{Trial: "together", Cores: []int{4, 2}, Regime: machine.R7, Workload: "all", DurationS: 900, Condition: machine.Together, Phase: journal.PhaseChecking})
			add(&journal.SMUReadback{Core: 2, Offset: -19})
			add(&journal.ConfigLoaded{Version: "new", Rev: "bbbb", Schema: schema, Ruleset: 6})
			add(&journal.TrialEnd{Trial: "together", Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 12})
			add(&journal.ProfileApplied{Offsets: []int{-9, -30}, Condition: machine.Parked})
			add(&journal.TrialIntent{Trial: "parked", Cores: []int{4, 2}, Regime: machine.R7, Workload: "all", DurationS: 120, Condition: machine.Parked, Phase: journal.PhaseHunt})
			add(&journal.TrialEnd{Trial: "parked", Outcome: journal.OutcomePass, DurationS: 120})
			add(&journal.TrialIntent{Trial: "deepening", Cores: []int{2, 4}, Profile: []int{-25, -35}, Regime: machine.R7, Workload: "all", DurationS: 120, Condition: machine.Together, Phase: journal.PhaseDeepening})
			add(&journal.TrialEnd{Trial: "deepening", Outcome: journal.OutcomePass, DurationS: 120})
			add(&journal.TrialIntent{Trial: "rerun", Core: new(2), Regime: machine.R2, Workload: "one", DurationS: 120, Condition: machine.Together, Phase: journal.PhaseChecking, Rerun: true})
			add(&journal.TrialEnd{Trial: "rerun", Outcome: journal.OutcomeFailure, Signal: machine.Stall, DurationS: 7})
			add(&journal.TrialIntent{Trial: "inconclusive", Core: new(2), Regime: machine.R2, Condition: machine.Alone})
			add(&journal.TrialEnd{Trial: "inconclusive", Outcome: journal.OutcomeInconclusive, DurationS: 1})
			add(&journal.TrialIntent{Trial: "open", Core: new(2), Regime: machine.R2, Condition: machine.Alone})
			events = append(events, journal.Event{Kind: "future.trial", Boot: "a"})
			events[7].Boot, events[8].Boot = "b", "b"
			writeJournal(t, path, events, `{"seq":21`)
			s, err := ReadJournal(path)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(&context, s.Context); diff != "" {
				t.Fatal(diff)
			}
			if s.ID != "20261001T000000Z" || s.Schema != schema || s.Ruleset != 1 || s.Archived || s.Path != path {
				t.Fatalf("session identity: %+v", s)
			}
			want := []Fact{
				{Kind: TrialFact, Session: s.ID, Seq: 5, Time: time.Unix(4, 0).UTC(), Build: build, Trial: "alone", Boot: "a", Class: Class{Regime: machine.R1, Workload: "one", Cores: []int{4}, DurationS: 90}, Condition: machine.Alone, Phase: journal.PhaseSearch, Profile: []int{0, -10}, Outcome: journal.OutcomePass, DurationS: 89},
				{Kind: TrialFact, Session: s.ID, Seq: 9, Time: time.Unix(8, 0).UTC(), Build: build, Trial: "together", Boot: "a", Class: Class{Regime: machine.R7, Workload: "all", Cores: []int{2, 4}, DurationS: 900}, Condition: machine.Together, Phase: journal.PhaseChecking, Profile: []int{-20, -30}, Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 12},
			}
			newBuild := journal.Build{Version: "new", Rev: "bbbb", Schema: schema, Ruleset: 6}
			for _, tc := range []struct {
				id        string
				seq       int
				condition machine.Condition
				phase     journal.Phase
				profile   []int
				outcome   journal.Outcome
				signal    machine.Signal
				duration  int
				rerun     bool
			}{
				{"parked", 12, machine.Parked, journal.PhaseHunt, []int{-9, -30}, journal.OutcomePass, "", 120, false},
				{"deepening", 14, machine.Together, journal.PhaseDeepening, []int{-25, -35}, journal.OutcomePass, "", 120, false},
				{"rerun", 16, machine.Together, journal.PhaseChecking, []int{-9, -30}, journal.OutcomeFailure, machine.Stall, 7, true},
			} {
				class := Class{Regime: machine.R7, Workload: "all", Cores: []int{2, 4}, DurationS: 120}
				if tc.rerun {
					class.Regime, class.Workload, class.Cores = machine.R2, "one", []int{2}
				}
				want = append(want, Fact{Kind: TrialFact, Session: s.ID, Seq: tc.seq, Time: time.Unix(int64(tc.seq-1), 0).UTC(), Build: newBuild, Trial: tc.id, Boot: "a", Class: class, Condition: tc.condition, Phase: tc.phase, Rerun: tc.rerun, Profile: tc.profile, Outcome: tc.outcome, Signal: tc.signal, DurationS: tc.duration})
			}
			if diff := cmp.Diff(want, s.Facts); diff != "" {
				t.Fatal(diff)
			}
			if diff := cmp.Diff([]journal.Build{build, newBuild}, s.Builds); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestIdleFailures(t *testing.T) {
	for _, tc := range []struct {
		name        string
		condition   machine.Condition
		attribution journal.Attribution
		trial       string
		profile     []int
		want        bool
	}{
		{"together", machine.Together, journal.Unattributed, "", []int{-10, -20}, true},
		{"parked", machine.Parked, journal.Unattributed, "", []int{-10, 0}, true},
		{"attributed", machine.Together, journal.Attributed, "", []int{-10, 0}, false},
		{"alone", machine.Alone, journal.Unattributed, "", []int{-10, 0}, false},
		{"stray", "", journal.Unattributed, "", nil, false},
		{"partial profile", machine.Together, journal.Unattributed, "", []int{-10}, false},
		{"trial failure is not idle", machine.Together, journal.Unattributed, "one", []int{-10, -20}, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "events.jsonl")
			build := journal.Build{Schema: 2, Ruleset: 6}
			at := time.Unix(10, 0).UTC()
			writeJournal(t, path, []journal.Event{
				{Data: &journal.SessionStart{Build: build, Session: "session", Cores: []machine.CoreInfo{{Core: 1}, {Core: 0}}}},
				{Time: at, Boot: "boot", Data: &journal.Failure{Signal: machine.Crash, Condition: tc.condition, Attribution: tc.attribution, Trial: tc.trial, Profile: tc.profile}},
			}, "")
			s, err := ReadJournal(path)
			if err != nil {
				t.Fatal(err)
			}
			var want []Fact
			if tc.want {
				want = []Fact{{Kind: IdleFact, Session: "session", Seq: 2, Time: at, Build: build, Epoch: 1, Boot: "boot", Class: Class{Regime: machine.R6, Cores: []int{0, 1}}, Condition: tc.condition, Profile: tc.profile, Outcome: journal.OutcomeFailure, Signal: machine.Crash, Idle: &IdleContext{Attribution: tc.attribution}}}
			}
			if diff := cmp.Diff(want, s.Facts); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestResetHistoryPreservesFacts(t *testing.T) {
	path := filepath.Join(t.TempDir(), "events.jsonl")
	events := []journal.Event{
		{Data: &journal.SessionStart{Schema: 1, Session: "session", Cores: []machine.CoreInfo{{Core: 0}}}},
		{Data: &journal.TrialIntent{Trial: "before", Core: new(0), Offset: new(-10), Condition: machine.Alone}},
		{Data: &journal.TrialEnd{Trial: "before", Outcome: journal.OutcomePass}},
		{Data: &journal.CommandReset{Core: new(0)}},
		{Cause: []int{4}, Data: &journal.CorePhase{Core: 0, To: journal.PhaseSearch, Offset: 0}},
		{Data: &journal.TrialIntent{Trial: "after", Core: new(0), Offset: new(-5), Condition: machine.Alone}},
		{Data: &journal.TrialEnd{Trial: "after", Outcome: journal.OutcomeFailure, Signal: machine.ComputationError}},
		{Data: &journal.CommandReset{All: true}},
		{Cause: []int{8}, Data: &journal.SessionArchived{Session: "session", Path: "archive/session.jsonl"}},
	}
	writeJournal(t, path, events, "")
	s, err := ReadJournal(path)
	if err != nil {
		t.Fatal(err)
	}
	want := []Reset{{Seq: 4, Core: new(0), AppliedSeq: 5, Effects: []journal.Event{s.Events[4]}}, {Seq: 8, All: true, AppliedSeq: 9, Effects: []journal.Event{s.Events[8]}}}
	if diff := cmp.Diff(want, s.Resets); diff != "" {
		t.Fatal(diff)
	}
	var got []string
	for _, f := range s.Facts {
		got = append(got, fmt.Sprintf("%s/%d/%v/%s", f.Trial, f.Build.Ruleset, f.Profile, f.Outcome))
	}
	if diff := cmp.Diff([]string{"before/1/[-10]/pass", "after/1/[-5]/failure"}, got); diff != "" {
		t.Fatal(diff)
	}
}

func TestReadDirNumericSessionOrder(t *testing.T) {
	dir := t.TempDir()
	archive := filepath.Join(dir, "archive")
	if err := os.Mkdir(archive, 0o700); err != nil {
		t.Fatal(err)
	}
	for _, id := range []string{"20261001T000000Z-10", "20261001T000000Z-2", "20261001T000000Z", "20260930T235959Z"} {
		writeJournal(t, filepath.Join(archive, id+".jsonl"), []journal.Event{{Data: &journal.SessionStart{Schema: 1, Session: id}}}, "")
	}
	if err := os.Mkdir(filepath.Join(archive, "20260901T000000Z-trials"), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(archive, "20260901T000000Z-carry-pending"), nil, 0o600); err != nil {
		t.Fatal(err)
	}
	writeJournal(t, filepath.Join(dir, "events.jsonl"), []journal.Event{{Data: &journal.SessionStart{Schema: 2, Ruleset: 6, Session: "20261001T000000Z-11"}}}, "")
	sessions, err := ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	var got []string
	for _, s := range sessions {
		got = append(got, fmt.Sprintf("%s/%t", s.ID, s.Archived))
	}
	want := []string{"20260930T235959Z/true", "20261001T000000Z/true", "20261001T000000Z-2/true", "20261001T000000Z-10/true", "20261001T000000Z-11/false"}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatal(diff)
	}
}

func TestHistoricalConfigurationAndReadErrors(t *testing.T) {
	for _, tc := range []struct {
		name, lines string
		wantError   bool
	}{
		{"old config", `{"seq":1,"kind":"session.start","schema":1,"session":"old"}
{"seq":2,"kind":"config.loaded","version":"v1","rev":"abc","config":{"durations":{"search_trial_s":"90s"},"guard":{"rotation":[{"regime":"R1"}]}}}
`, false},
		{"unknown kind", `{"seq":1,"kind":"session.start","schema":2}
{"seq":2,"kind":"future.fact","trial":false,"outcome":42}
`, false},
		{"malformed complete line", `{"seq":1,"kind":"session.start","schema":2}
not-json
`, true},
		{"missing kind", `{"seq":1,"schema":2}
`, true},
		{"newer schema", fmt.Sprintf("{\"seq\":1,\"kind\":\"session.start\",\"schema\":%d}\n", journal.Schema+1), true},
		{"bad sequence", `{"seq":1,"kind":"session.start","schema":2}
{"seq":3,"kind":"future.fact"}
`, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "events.jsonl")
			if err := os.WriteFile(path, []byte(tc.lines), 0o600); err != nil {
				t.Fatal(err)
			}
			s, err := ReadJournal(path)
			if (err != nil) != tc.wantError {
				t.Fatalf("ReadJournal error = %v, want error %t", err, tc.wantError)
			}
			if tc.name == "old config" {
				if diff := cmp.Diff([]journal.Build{{Schema: 1, Ruleset: 1}, {Version: "v1", Rev: "abc", Ruleset: 1}}, s.Builds); diff != "" {
					t.Fatal(diff)
				}
			}
			if tc.name == "unknown kind" {
				if len(s.Events) != 2 || s.Events[1].Kind != "future.fact" || len(s.Facts) != 0 {
					t.Fatalf("unknown event must remain available without inventing facts: %+v", s)
				}
			}
		})
	}
}

func TestUnstampedConfigKeepsRecordedBuild(t *testing.T) {
	t.Parallel()
	build := journal.Build{Schema: 1, Ruleset: 1}
	events := []journal.Event{
		{Seq: 1, Data: &journal.SessionStart{Build: build, Session: "old", Cores: []machine.CoreInfo{{Core: 0}}}},
		{Seq: 2, Data: &journal.ConfigLoaded{}},
		{Seq: 3, Data: &journal.TrialIntent{Trial: "one", Core: new(0), Offset: new(-10), Regime: machine.R1, Workload: "one", DurationS: 90, Condition: machine.Alone}},
		{Seq: 4, Data: &journal.TrialEnd{Trial: "one", Outcome: journal.OutcomePass, DurationS: 90}},
	}
	s := FromEvents(events)
	if diff := cmp.Diff([]journal.Build{build}, s.Builds); diff != "" {
		t.Fatalf("recorded builds (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff(build, s.Facts[0].Build); diff != "" {
		t.Fatalf("trial provenance (-want +got):\n%s", diff)
	}
}

func TestTrialEvidenceTracksOnlyOpenStartedTrialInItsBoot(t *testing.T) {
	at := time.Unix(100, 0).UTC()
	for _, kind := range []string{"progress", "signal", "sample"} {
		for _, state := range []string{"running", "not started", "ended", "other boot", "unknown trial"} {
			t.Run(kind+"/"+state, func(t *testing.T) {
				events := []journal.Event{
					{Seq: 1, Boot: "a", Data: &journal.SessionStart{Schema: 2, Ruleset: 6, Session: "session", Cores: []machine.CoreInfo{{Core: 0}}}},
					{Seq: 2, Time: at, Boot: "a", Data: &journal.TrialIntent{Trial: "one", Core: new(0), Offset: new(-10), Regime: machine.R1, Condition: machine.Alone, Profile: []int{-10}, DurationS: 90}},
				}
				want := at
				if state != "not started" {
					events = append(events, journal.Event{Seq: 3, Time: at.Add(time.Second), Boot: "a", Data: &journal.TrialStart{Trial: "one"}})
					want = at.Add(time.Second)
				}
				if state == "ended" {
					events = append(events, journal.Event{Seq: len(events) + 1, Boot: "a", Data: &journal.TrialEnd{Trial: "one", Outcome: journal.OutcomeInconclusive}})
				}
				id, boot := "one", "a"
				if state == "unknown trial" {
					id = "other"
				}
				if state == "other boot" {
					boot = "b"
				}
				var payload journal.Payload
				switch kind {
				case "progress":
					payload = &journal.TrialProgress{Trial: id}
				case "signal":
					payload = &journal.TrialSignal{Trial: id}
				case "sample":
					payload = &journal.TrialSample{Trial: id}
				}
				events = append(events, journal.Event{Seq: len(events) + 1, Time: at.Add(10 * time.Second), Boot: boot, Data: payload})
				if state == "running" {
					want = at.Add(10 * time.Second)
				}
				s := FromEvents(events)
				if diff := cmp.Diff(want, s.Trials[0].LastEvidence); diff != "" {
					t.Fatalf("last trial evidence (-want +got):\n%s", diff)
				}
				if s.Trials[0].Started != (state != "not started") || len(s.Facts) != 0 {
					t.Fatalf("interrupted work became decisive evidence: %+v", s)
				}
			})
		}
	}
}

func TestCrashEvidenceMatchesIntentSequenceAndPreviousBoot(t *testing.T) {
	for _, tc := range []struct {
		name     string
		inFlight *int
		previous string
		want     bool
	}{
		{"matching", new(2), "a", true},
		{"no in flight", nil, "a", false},
		{"unknown intent", new(99), "a", false},
		{"different boot", new(2), "b", false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			s := FromEvents([]journal.Event{
				{Seq: 1, Boot: "a", Data: &journal.SessionStart{Schema: 2, Ruleset: 6, Session: "session", Cores: []machine.CoreInfo{{Core: 0}}}},
				{Seq: 2, Boot: "a", Data: &journal.TrialIntent{Trial: "one", Core: new(0), Offset: new(-10), Regime: machine.R1, Condition: machine.Alone, Profile: []int{-10}, DurationS: 90}},
				{Seq: 3, Boot: "c", Data: &journal.CrashDetected{InFlight: tc.inFlight, PreviousBoot: tc.previous}},
			})
			if s.Trials[0].Crashed != tc.want || len(s.Facts) != 0 {
				t.Fatalf("crash association: crashed %t, facts %+v", s.Trials[0].Crashed, s.Facts)
			}
		})
	}
}

func TestReadDirRejectsBrokenSources(t *testing.T) {
	for _, source := range []string{"archive directory", "archive journal", "live journal"} {
		t.Run(source, func(t *testing.T) {
			dir := t.TempDir()
			path := filepath.Join(dir, "archive")
			if source != "archive directory" {
				if err := os.Mkdir(path, 0o700); err != nil {
					t.Fatal(err)
				}
				path = filepath.Join(path, "old.jsonl")
				if source == "live journal" {
					path = filepath.Join(dir, "events.jsonl")
				}
			}
			if err := os.WriteFile(path, []byte("not JSON\n"), 0o600); err != nil {
				t.Fatal(err)
			}
			sessions, err := ReadDir(dir)
			if err == nil || sessions != nil {
				t.Fatalf("broken source silently accepted: sessions %+v, error %v", sessions, err)
			}
		})
	}
}
