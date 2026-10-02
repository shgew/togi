package journal

import (
	"errors"
	"fmt"
	"testing"
)

func TestEvidenceEpochCompatibility(t *testing.T) {
	binary := Build{Schema: 2, Ruleset: 6, EvidenceEpoch: 2}
	for _, tc := range []struct {
		name     string
		recorded Build
		older    bool
		field    string
	}{
		{"older epoch", Build{Schema: 2, Ruleset: 6, EvidenceEpoch: 1}, true, "evidence epoch"},
		{"unstamped ruleset six", Build{Schema: 2, Ruleset: 6}, true, "evidence epoch"},
		{"current epoch", binary, false, ""},
		{"newer epoch", Build{Schema: 2, Ruleset: 6, EvidenceEpoch: 3}, false, "evidence epoch"},
		{"newer epoch with older ruleset", Build{Schema: 2, Ruleset: 5, EvidenceEpoch: 3}, false, "ruleset"},
		{"newer epoch with older schema", Build{Schema: 1, Ruleset: 6, EvidenceEpoch: 3}, false, "schema"},
		{"older epoch with newer ruleset", Build{Schema: 2, Ruleset: 7, EvidenceEpoch: 1}, false, "ruleset"},
		{"older epoch with newer schema", Build{Schema: 3, Ruleset: 6, EvidenceEpoch: 1}, false, "schema"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := Older(tc.recorded, binary); got != tc.older {
				t.Fatalf("Older(%+v, %+v) = %t, want %t", tc.recorded, binary, got, tc.older)
			}
			err := Compatible(tc.recorded, binary)
			if tc.field == "" {
				if err != nil {
					t.Fatal(err)
				}
				return
			}
			var incompatible *IncompatibleError
			if !errors.As(err, &incompatible) || incompatible.Field != tc.field {
				t.Fatalf("Compatible = %v, want %s mismatch", err, tc.field)
			}
		})
	}
	if err := Compatible(Build{Schema: 2, Ruleset: 6, EvidenceEpoch: 1}, Build{Schema: 2, Ruleset: 6}); err != nil {
		t.Fatalf("unstamped binary build must default to epoch one: %v", err)
	}
}

func TestSessionEvidenceEpochSurvivesResumeStamps(t *testing.T) {
	for _, tc := range []struct {
		ruleset, evidence, want int
	}{
		{5, 0, 0},
		{6, 0, 1},
		{7, 0, 1},
		{5, 2, 2},
		{6, 2, 2},
	} {
		t.Run(fmt.Sprintf("ruleset-%d-evidence-%d", tc.ruleset, tc.evidence), func(t *testing.T) {
			data := []byte(fmt.Sprintf(`{"kind":"session.start","session":"source","schema":2,"ruleset":%d,"evidence":%d,"version":"old"}`+"\n"+`{"kind":"config.loaded","version":"new","evidence":99}`+"\n", tc.ruleset, tc.evidence))
			stamp, _, err := scanBuild(data)
			if err != nil {
				t.Fatal(err)
			}
			decoded := BuildOf([]Event{
				{Data: &SessionStart{Build: Build{Schema: 2, Ruleset: tc.ruleset, Version: "old"}, Evidence: tc.evidence}},
				{Data: &ConfigLoaded{Build: Build{Version: "new", EvidenceEpoch: 99}}},
			})
			if stamp.Epoch() != tc.want || decoded.Epoch() != tc.want || stamp.Version != "new" || decoded.Version != "new" {
				t.Fatalf("resume must retain session epoch %d: scan %+v, decoded %+v", tc.want, stamp, decoded)
			}
		})
	}
}
