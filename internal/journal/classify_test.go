package journal

import (
	"errors"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
)

var allOperations = []Operation{OpResume, OpAppend, OpResetAll, OpInspect, OpReplay, OpHistory}

// access lists the permission of each of allOperations, in that order.
func access(resume, appendOp, resetAll, inspect, replay, history Access) []Access {
	return []Access{resume, appendOp, resetAll, inspect, replay, history}
}

func TestClassifyPermissions(t *testing.T) {
	const (
		R = AccessRefuse
		U = AccessUse
		A = AccessArchive
	)
	binary := Build{Schema: 4, Ruleset: 7, EvidenceEpoch: 2}
	for _, tc := range []struct {
		name     string
		binary   Build
		recorded Build
		field    string // first differing dimension; empty when current
		want     []Access
	}{
		{"current", binary, Build{Schema: 4, Ruleset: 7, EvidenceEpoch: 2}, "", access(U, U, U, U, U, U)},
		{"older ruleset", binary, Build{Schema: 4, Ruleset: 6, EvidenceEpoch: 2}, "ruleset", access(A, R, U, U, R, U)},
		{"newer ruleset", binary, Build{Schema: 4, Ruleset: 8, EvidenceEpoch: 2}, "ruleset", access(R, R, U, U, R, U)},
		{"older schema", binary, Build{Schema: 3, Ruleset: 7, EvidenceEpoch: 2}, "schema", access(A, R, A, R, U, U)},
		{"newer schema", binary, Build{Schema: 5, Ruleset: 7, EvidenceEpoch: 2}, "schema", access(R, R, A, R, R, R)},
		{"older epoch", binary, Build{Schema: 4, Ruleset: 7, EvidenceEpoch: 1}, "evidence epoch", access(A, R, U, U, U, U)},
		{"newer epoch", binary, Build{Schema: 4, Ruleset: 7, EvidenceEpoch: 3}, "evidence epoch", access(R, R, U, U, U, U)},
		{"older schema, newer ruleset", binary, Build{Schema: 3, Ruleset: 8, EvidenceEpoch: 2}, "schema", access(R, R, A, R, R, U)},
		{"newer schema, older ruleset", binary, Build{Schema: 5, Ruleset: 6, EvidenceEpoch: 2}, "schema", access(R, R, A, R, R, R)},
		{"older schema, newer epoch", binary, Build{Schema: 3, Ruleset: 7, EvidenceEpoch: 3}, "schema", access(R, R, A, R, U, U)},
		{"older ruleset, newer epoch", binary, Build{Schema: 4, Ruleset: 6, EvidenceEpoch: 3}, "ruleset", access(R, R, U, U, R, U)},
		{"newer ruleset, older epoch", binary, Build{Schema: 4, Ruleset: 8, EvidenceEpoch: 1}, "ruleset", access(R, R, U, U, R, U)},
		{"every dimension older", binary, Build{Schema: 3, Ruleset: 6, EvidenceEpoch: 1}, "schema", access(A, R, A, R, R, U)},
		{"every dimension newer", binary, Build{Schema: 5, Ruleset: 8, EvidenceEpoch: 3}, "schema", access(R, R, A, R, R, R)},
		{"unstamped ruleset is ruleset 1", binary, Build{Schema: 4}, "ruleset", access(A, R, U, U, R, U)},
		{"unstamped ruleset 1 is current under ruleset 1", Build{Schema: 4, Ruleset: 1}, Build{Schema: 4}, "", access(U, U, U, U, U, U)},
		{"unstamped epoch defaults to 1 from ruleset 6", Build{Schema: 4, Ruleset: 6}, Build{Schema: 4, Ruleset: 6, EvidenceEpoch: 1}, "", access(U, U, U, U, U, U)},
		{"unstamped epoch is older than 1 before ruleset 6", Build{Schema: 4, Ruleset: 5, EvidenceEpoch: 1}, Build{Schema: 4, Ruleset: 5}, "evidence epoch", access(A, R, U, U, U, U)},
		{"explicit epoch wins over the ruleset default", Build{Schema: 4, Ruleset: 6}, Build{Schema: 4, Ruleset: 6, EvidenceEpoch: 2}, "evidence epoch", access(R, R, U, U, U, U)},
	} {
		t.Run(tc.name, func(t *testing.T) {
			c := Classify(tc.recorded, tc.binary)
			var got []Access
			for _, op := range allOperations {
				got = append(got, c.Access(op))
			}
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Fatalf("access for resume, append, reset-all, inspect, replay, history (-want +got):\n%s", diff)
			}
			err := c.Err()
			if tc.field == "" {
				if err != nil || !c.Current() {
					t.Fatalf("current journal: Err %v, Current %t", err, c.Current())
				}
				return
			}
			var incompatible *IncompatibleError
			if !errors.As(err, &incompatible) || incompatible.Field != tc.field {
				t.Fatalf("Err = %v, want %s mismatch", err, tc.field)
			}
			if c.Current() {
				t.Fatal("differing journal reported current")
			}
		})
	}
}

func TestClassifyDirections(t *testing.T) {
	binary := Build{Schema: 4, Ruleset: 7, EvidenceEpoch: 2}
	c := Classify(Build{Schema: 5, Ruleset: 6, EvidenceEpoch: 2}, binary)
	if c.Schema != DirNewer || c.Ruleset != DirOlder || c.Epoch != DirSame || !c.Newer() || c.Older() {
		t.Fatalf("mixed directions: %+v", c)
	}
	c = Classify(Build{Schema: 3, Ruleset: 7, EvidenceEpoch: 2}, binary)
	if c.Schema != DirOlder || c.Ruleset != DirSame || c.Epoch != DirSame || c.Newer() || !c.Older() {
		t.Fatalf("older schema: %+v", c)
	}
}

func TestClassifyUnknownKinds(t *testing.T) {
	binary := Build{Schema: 4, Ruleset: 7, EvidenceEpoch: 2}
	for _, tc := range []struct {
		name     string
		recorded Build
		refused  []Operation
	}{
		{"current schema, ruleset and epoch", Build{Schema: 4, Ruleset: 7, EvidenceEpoch: 2}, []Operation{OpResume, OpAppend, OpResetAll}},
		{"older ruleset", Build{Schema: 4, Ruleset: 6, EvidenceEpoch: 2}, []Operation{OpAppend, OpResetAll}},
		{"older epoch", Build{Schema: 4, Ruleset: 7, EvidenceEpoch: 1}, []Operation{OpAppend, OpResetAll}},
		{"older schema", Build{Schema: 3, Ruleset: 7, EvidenceEpoch: 2}, []Operation{OpAppend}},
		{"older schema and ruleset", Build{Schema: 3, Ruleset: 6, EvidenceEpoch: 2}, []Operation{OpAppend}},
		{"newer ruleset", Build{Schema: 4, Ruleset: 8, EvidenceEpoch: 2}, []Operation{OpResume, OpAppend, OpResetAll}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			events := []Event{{Kind: KindSessionStart, Data: &SessionStart{Build: tc.recorded, Evidence: tc.recorded.EvidenceEpoch}}, {Kind: "future.fact"}}
			c := Classify(BuildOf(events), binary)
			for _, op := range allOperations {
				err := c.Kinds(op, events)
				want := false
				for _, refused := range tc.refused {
					want = want || refused == op
				}
				var unknown *UnknownKindError
				if got := errors.As(err, &unknown); got != want {
					t.Errorf("operation %d: Kinds = %v, refused %t, want %t", op, err, got, want)
				}
				if want && (unknown.Kind != "future.fact" || unknown.Binary != binary) {
					t.Errorf("operation %d: refusal %+v", op, unknown)
				}
			}
		})
	}
}

func TestLockedReadReusesDecodeUntilFileChanges(t *testing.T) {
	dir := t.TempDir()
	j := openTest(t, dir)
	appendAll(t, j, []Payload{sessionStart()})
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	locked, err := Lock(dir, Options{})
	if err != nil {
		t.Fatal(err)
	}
	defer locked.Close()
	first, _, err := locked.Read()
	if err != nil {
		t.Fatal(err)
	}
	second, _, err := locked.Read()
	if err != nil || len(second) != 1 || &first[0] != &second[0] {
		t.Fatalf("unchanged file was decoded again: %v", err)
	}
	f, err := os.OpenFile(filepath.Join(dir, eventsFile), os.O_APPEND|os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := f.WriteString(`{"seq":2,"kind":"shutdown","reason":"command"}` + "\n"); err != nil {
		t.Fatal(err)
	}
	if err := f.Close(); err != nil {
		t.Fatal(err)
	}
	third, _, err := locked.Read()
	if err != nil || len(third) != 2 {
		t.Fatalf("changed file: %d events, %v", len(third), err)
	}
}
