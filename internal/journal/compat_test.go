package journal

import (
	"bytes"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
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

func TestRefusalNamesTheProductABuildWasReleasedAs(t *testing.T) {
	binary := Build{Version: "0.4.0", Schema: 2, Ruleset: 4}
	for _, tc := range []struct{ version, want string }{
		{"", "Install the shycler build that wrote it"},
		{"0.3.1", "Install shycler 0.3.1"},
		{"0.1.0+dev", "Install shycler 0.1.0+dev"},
		{"0.4.0", "Install togi 0.4.0"},
		{"0.10.2", "Install togi 0.10.2"},
		{"1.0.0", "Install togi 1.0.0"},
	} {
		err := Compatible(Build{Version: tc.version, Schema: 2, Ruleset: 3}, binary)
		if err == nil || !strings.Contains(err.Error(), tc.want) || !strings.Contains(err.Error(), "this build, togi 0.4.0,") {
			t.Errorf("version %q: %v, want %q", tc.version, err, tc.want)
		}
	}
}

func TestScanBuildSkipsUnknownSchemaPayloads(t *testing.T) {
	data := []byte(`{"kind":"session.start","session":"old","schema":42,"ruleset":99,"version":"0.1.0","rev":"old"}` + "\n" +
		`{"kind":"later.unknown","schema":43,"version":"ignored"}` + "\n" +
		`{"kind":"config.loaded","schema":55,"version":"0.2.1","rev":"def5678","fixes":3,"unreadable_payload":true}` + "\n" +
		`{"kind":"config.loaded","schema":66,"version":"0.3.0","rev":"abc1234","fixes":4}` + "\n")
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
