package journal

import (
	"bytes"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestCompatible(t *testing.T) {
	binary := Build{Version: "0.3.0", Rev: "abc1234", Schema: 1, Ruleset: 3}
	for _, tc := range []struct {
		name, field string
		journal     Build
	}{
		{"same build", "", binary},
		{"different version and fixes remain compatible", "", Build{Version: "0.2.1", Rev: "def5678", Schema: 1, Ruleset: 3, Fixes: 4}},
		{"different ruleset", "ruleset", Build{Version: "0.2.1", Rev: "def5678", Schema: 1, Ruleset: 2}},
		{"schema takes precedence", "schema", Build{Version: "0.2.1", Schema: 2, Ruleset: 2}},
		{"old unstamped session", "ruleset", Build{Schema: 1}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			err := Compatible(tc.journal, binary)
			if tc.field == "" {
				if err != nil {
					t.Fatal(err)
				}
				return
			}
			var incompatible *IncompatibleError
			if !errors.As(err, &incompatible) || incompatible.Field != tc.field {
				t.Fatalf("Compatible: %v, want %s mismatch", err, tc.field)
			}
			if tc.journal.Version == "" && (!strings.Contains(err.Error(), "before version stamps") || incompatible.Journal.Ruleset != 1 || incompatible.Journal.Fixes != 0) {
				t.Fatalf("old journal: %+v: %v", incompatible.Journal, err)
			}
		})
	}
	if err := Compatible(Build{Schema: 1}, Build{Schema: 1, Ruleset: 1}); err != nil {
		t.Fatalf("ruleset 1 must accept an unstamped session: %v", err)
	}
}

func TestRefusalOfAnOlderJournalNamesTheCarry(t *testing.T) {
	err := Compatible(Build{Version: "0.4.0", Schema: 2, Ruleset: 3}, Build{Version: "0.5.0", Schema: 2, Ruleset: 4})
	want := "this journal was written by togi 0.4.0 (schema 2, ruleset 3); this build, togi 0.5.0, uses ruleset 4. togi run archives it and starts a new session that carries its candidate solo limits and failure points; togi reset --all archives it and starts over."
	if err == nil || err.Error() != want {
		t.Fatalf("Compatible: %v, want %q", err, want)
	}
}

func TestOlder(t *testing.T) {
	binary := Build{Schema: 2, Ruleset: 3}
	for _, tc := range []struct {
		name     string
		recorded Build
		want     bool
	}{
		{"older ruleset", Build{Schema: 2, Ruleset: 2}, true},
		{"older schema and ruleset", Build{Schema: 1, Ruleset: 1}, true},
		{"same build", Build{Schema: 2, Ruleset: 3}, false},
		{"newer ruleset", Build{Schema: 2, Ruleset: 4}, false},
		{"newer schema, older ruleset", Build{Schema: 3, Ruleset: 1}, false},
		{"unstamped ruleset", Build{Schema: 2}, true},
	} {
		if got := Older(tc.recorded, binary); got != tc.want {
			t.Errorf("%s: Older(%+v) = %v, want %v", tc.name, tc.recorded, got, tc.want)
		}
	}
}

func TestScanBuildSkipsUnknownSchemaPayloads(t *testing.T) {
	data := []byte(`{"kind":"session.start","session":"old","schema":42,"ruleset":99,"version":"0.1.0","rev":"old"}` + "\n" +
		`{"kind":"later.unknown","schema":43,"version":"ignored"}` + "\n" +
		`{"kind":"config.loaded","schema":55,"version":"0.2.1","rev":"def5678","fixes":3,"unreadable_payload":true}` + "\n" +
		`{"kind":"config\u002eloaded","schema":66,"version":"0.3.0","rev":"abc1234","fixes":4}` + "\n")
	stamp, id, err := scanBuild(data)
	if err != nil || id != "old" || stamp.Schema != 42 || stamp.Ruleset != 99 || stamp.Version != "0.3.0" || stamp.Rev != "abc1234" || stamp.Fixes != 4 {
		t.Fatalf("scan: %+v, id %q, error %v", stamp, id, err)
	}
	dir := t.TempDir()
	path := filepath.Join(dir, eventsFile)
	if err := os.WriteFile(path, data, 0o644); err != nil {
		t.Fatal(err)
	}
	_, err = Open(dir, Options{Boot: "next", Now: fixedClock()})
	var incompatible *IncompatibleError
	if !errors.As(err, &incompatible) || incompatible.Field != "schema" || incompatible.Journal.Version != "0.3.0" {
		t.Fatalf("Open: %v", err)
	}
	_, _, err = ReadFile(path)
	if !errors.As(err, &incompatible) || incompatible.Field != "schema" {
		t.Fatalf("ReadFile: %v", err)
	}
	after, err := os.ReadFile(path)
	if err != nil || !bytes.Equal(after, data) {
		t.Fatalf("incompatible journal changed: %v", err)
	}
}

func TestBuildOfUsesLastStampedResume(t *testing.T) {
	first := &SessionStart{Schema: 1, Ruleset: 1, Version: "0.1.0"}
	old := &ConfigLoaded{}
	latest := &ConfigLoaded{Version: "0.2.1", Rev: "def5678", Fixes: 2}
	got := BuildOf([]Event{{Data: first}, {Data: latest}, {Data: old}})
	if got.Version != latest.Version || got.Rev != latest.Rev || got.Fixes != 2 || got.Schema != 1 || got.Ruleset != 1 {
		t.Fatalf("last stamp: %+v", got)
	}
}

func TestScanDamagedOrTornStamp(t *testing.T) {
	for _, data := range []string{"{\n", `{"kind":"shutdown"}` + "\n"} {
		if _, _, err := scanBuild([]byte(data)); err == nil {
			t.Fatalf("invalid stamp accepted: %q", data)
		}
	}
	if stamp, id, err := scanBuild([]byte(`{"kind":"session.start"`)); err != nil || id != "" || stamp != (Build{}) {
		t.Fatalf("torn first stamp: %+v, %q, %v", stamp, id, err)
	}
	stamp, id, err := scanBuild([]byte(`{"kind":"session.start","session":"source","schema":2}` + "\n" + `{"kind":"config.loaded","version":"torn"`))
	if err != nil || id != "source" || stamp.Ruleset != 1 || stamp.Version != "" {
		t.Fatalf("torn resume stamp changed identity: %+v, %q, %v", stamp, id, err)
	}
	got := BuildOf([]Event{{Data: &SessionStart{Schema: Schema}}})
	if got.Ruleset != 1 {
		t.Fatalf("unstamped ruleset = %d, want 1", got.Ruleset)
	}
}

func TestEvidenceEpochCompatibilityDiagnostics(t *testing.T) {
	t.Parallel()
	binary := Build{Version: "new", Schema: Schema, Ruleset: 7, EvidenceEpoch: 2}
	for _, tc := range []struct {
		name     string
		recorded Build
		want     string
	}{
		{"older epoch", Build{Version: "old", Schema: Schema, Ruleset: 7, EvidenceEpoch: 1}, "this journal was written by togi old (schema 4, ruleset 7, evidence epoch 1); this build, togi new, uses evidence epoch 2. togi run archives it and starts a new session that carries its candidate solo limits and failure points; togi reset --all archives it and starts over."},
		{"newer epoch", Build{Version: "future", Schema: Schema, Ruleset: 7, EvidenceEpoch: 3}, "this journal was written by togi future (schema 4, ruleset 7, evidence epoch 3); this build, togi new, uses evidence epoch 2. Install togi future to continue this session, or run togi reset --all to archive it and start over."},
	} {
		t.Run(tc.name, func(t *testing.T) {
			err := Compatible(tc.recorded, binary)
			if err == nil {
				t.Fatal("incompatible epoch accepted")
			}
			if diff := cmp.Diff(tc.want, err.Error()); diff != "" {
				t.Fatalf("epoch diagnostic (-want +got):\n%s", diff)
			}
		})
	}
}

func TestRulesetWarningNamesBothStrategies(t *testing.T) {
	want := "warning: journal written by togi old (ruleset 6); rendered with this build's rules (ruleset 7)"
	got := RulesetWarning(Build{Version: "old", Ruleset: 6}, Build{Ruleset: 7})
	if diff := cmp.Diff(want, got); diff != "" {
		t.Fatalf("ruleset warning (-want +got):\n%s", diff)
	}
}
