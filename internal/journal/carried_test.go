package journal

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func TestEvidenceEpochDefaults(t *testing.T) {
	for _, tc := range []struct {
		name     string
		ruleset  int
		evidence int
		want     int
	}{
		{"unstamped", 0, 0, 0},
		{"old ruleset", 5, 0, 0},
		{"first compatible ruleset", 6, 0, 1},
		{"later ruleset", 8, 0, 1},
		{"explicit old ruleset", 5, 3, 3},
		{"explicit current ruleset", 6, 4, 4},
	} {
		t.Run(tc.name, func(t *testing.T) {
			payload := &SessionStart{Schema: Schema, Ruleset: tc.ruleset, Session: "session", Evidence: tc.evidence}
			raw, err := encode(Event{Seq: 1, Kind: KindSessionStart, Time: time.Unix(0, 0), Data: payload})
			if err != nil {
				t.Fatal(err)
			}
			event, err := decode(raw)
			if err != nil {
				t.Fatal(err)
			}
			if got := event.Data.(*SessionStart).Epoch(); got != tc.want {
				t.Fatalf("epoch = %d, want %d", got, tc.want)
			}
		})
	}
}

func TestCarriedPayloadsDecodeAcrossReaders(t *testing.T) {
	source := FactSource{Session: "original", Seq: 304, Build: Build{Version: "0.7.0", Rev: "rev", Schema: 2, Ruleset: 6, Fixes: 2}, Trial: "0304", Time: time.Unix(100, 0).UTC(), Boot: "original-boot", Evidence: 3}
	class := TrialClass{Regime: machine.R7, Workload: "workload", Cores: []int{3, 7}, DurationS: 120}
	payloads := []Payload{
		&TrialCarried{Source: source, Class: class, Condition: machine.Resident, Phase: PhaseGuard, Rerun: true, RecordOnly: true, Profile: []int{-10, -30, -50}, Outcome: OutcomeFailure, Signal: machine.ComputationError, DurationS: 11, Core: new(7)},
		&FailureCarried{Source: FactSource{Session: source.Session, Seq: 400, Build: source.Build, Time: source.Time, Boot: source.Boot, Evidence: 0}, Class: TrialClass{Regime: machine.R6, Cores: []int{1, 3, 7}}, Signal: machine.Crash, Attribution: Unattributed, Core: new(3), Offset: new(-30), Regime: machine.R6, Condition: machine.Masked, Profile: []int{-10, -30, -50}},
	}
	for _, schema := range []int{1, Schema} {
		for _, payload := range payloads {
			t.Run(fmt.Sprintf("%s/schema-%d", payload.Kind(), schema), func(t *testing.T) {
				path := filepath.Join(t.TempDir(), "events.jsonl")
				start, err := encode(Event{Seq: 1, Kind: KindSessionStart, Time: time.Unix(200, 0), Data: &SessionStart{Schema: schema, Ruleset: 6, Session: "copy"}})
				if err != nil {
					t.Fatal(err)
				}
				raw, err := encode(Event{Seq: 2, Kind: payload.Kind(), Time: time.Unix(201, 0), Boot: "copy-boot", Msg: payload.Message(), Data: payload})
				if err != nil {
					t.Fatal(err)
				}
				if err := os.WriteFile(path, append(append(append(start, '\n'), raw...), '\n'), 0600); err != nil {
					t.Fatal(err)
				}
				readers := []func(string) ([]Event, error){ReadHistory, ReadForCarry}
				if schema == Schema {
					readers = append(readers, func(path string) ([]Event, error) {
						events, _, err := ReadFile(path)
						return events, err
					})
				}
				for _, reader := range readers {
					events, err := reader(path)
					if err != nil {
						t.Fatal(err)
					}
					if diff := cmp.Diff(payload, events[1].Data); diff != "" {
						t.Fatalf("carried payload changed: %s", diff)
					}
					if !(Filter{Core: new(3)}).Match(events[1]) || (Filter{Core: new(2)}).Match(events[1]) {
						t.Fatal("carried event core filter ignored loaded cores")
					}
					if payload.Kind() == KindTrialCarried && (!(Filter{Trial: "0304", Kinds: []string{"trial"}}).Match(events[1]) || (Filter{Trial: "other"}).Match(events[1])) {
						t.Fatal("carried trial filter ignored source identity")
					}
					if payload.Kind() == KindFailureCarried && !(Filter{Kinds: []string{"failure"}}).Match(events[1]) {
						t.Fatal("carried failure missing from failure group")
					}
				}
			})
		}
	}
}

func TestRecordOnlyMarkerRoundTripAndLegacyDefault(t *testing.T) {
	for _, recordOnly := range []bool{false, true} {
		for _, payload := range []Payload{
			&TrialIntent{Trial: "partial", Cores: []int{3}, Regime: machine.R7, Condition: machine.Resident, Rotation: 2, Step: 4, RecordOnly: recordOnly},
			&TrialCarried{Source: FactSource{Session: "original", Trial: "partial"}, Class: TrialClass{Regime: machine.R7, Cores: []int{3}}, Condition: machine.Resident, Outcome: OutcomeFailure, RecordOnly: recordOnly},
		} {
			t.Run(fmt.Sprintf("%s/record-only-%t", payload.Kind(), recordOnly), func(t *testing.T) {
				raw, err := encode(Event{Seq: 1, Kind: payload.Kind(), Data: payload})
				if err != nil {
					t.Fatal(err)
				}
				if got := bytes.Contains(raw, []byte(`"record_only":true`)); got != recordOnly {
					t.Fatalf("marker in JSON = %v, want %v: %s", got, recordOnly, raw)
				}
				if !recordOnly && bytes.Contains(raw, []byte(`"record_only"`)) {
					t.Fatalf("legacy-compatible JSON contains false marker: %s", raw)
				}
				event, err := decode(raw)
				if err != nil {
					t.Fatal(err)
				}
				if diff := cmp.Diff(payload, event.Data); diff != "" {
					t.Fatalf("marker roundtrip (-want +got):\n%s", diff)
				}
			})
		}
	}
	event, err := decode([]byte(`{"seq":1,"kind":"trial.intent","trial":"old"}`))
	if err != nil {
		t.Fatal(err)
	}
	if in := event.Data.(*TrialIntent); in.RecordOnly || in.Step != 0 {
		t.Fatalf("preexisting intent acquired partial fields: %+v", in)
	}
}

func TestGuardStepSnapshotRoundTrip(t *testing.T) {
	dir := t.TempDir()
	j, err := Open(dir, Options{Boot: "boot", Now: func() time.Time { return time.Unix(100, 0).UTC() }})
	if err != nil {
		t.Fatal(err)
	}
	defer j.Close()
	if _, err := j.Append(&SessionStart{Schema: Schema, Ruleset: 8, Session: "current"}); err != nil {
		t.Fatal(err)
	}
	snapshot := &GuardStep{Rotation: 2, Step: 4, Profile: []int{-20, -50, -30, -50}, Partials: []GuardPartial{
		{CCD: 1, Cores: []int{3}},
		{CCD: 5, Cores: []int{}, Reason: "all CCD cores are done"},
	}}
	if _, err := j.Append(snapshot); err != nil {
		t.Fatal(err)
	}
	if err := ValidateKindSelector("guard.step"); err != nil {
		t.Fatal(err)
	}
	for _, reader := range []func(string) ([]Event, error){
		ReadHistory,
		func(path string) ([]Event, error) {
			events, _, err := ReadFile(path)
			return events, err
		},
	} {
		events, err := reader(filepath.Join(dir, "events.jsonl"))
		if err != nil {
			t.Fatal(err)
		}
		if events[1].Kind != KindGuardStep {
			t.Fatalf("snapshot kind = %s", events[1].Kind)
		}
		if diff := cmp.Diff(snapshot, events[1].Data); diff != "" {
			t.Fatalf("frozen step snapshot (-want +got):\n%s", diff)
		}
	}
}
