package journal

import (
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
)

func TestFilterIntersectsKindAndTime(t *testing.T) {
	at := time.Unix(100, 0)
	for _, tc := range []struct {
		name   string
		filter Filter
		raw    string
		want   bool
	}{
		{"exact kind", Filter{Kinds: []string{"trial.end"}}, "", true},
		{"different exact kind", Filter{Kinds: []string{"trial.intent"}}, "", false},
		{"different group", Filter{Kinds: []string{"failure"}}, "", false},
		{"since inclusive", Filter{Since: at}, "", true},
		{"before since", Filter{Since: at.Add(time.Second)}, "", false},
		{"until exclusive", Filter{Until: at}, "", false},
		{"before until", Filter{Until: at.Add(time.Second)}, "", true},
		{"intersection", Filter{Kinds: []string{"failure"}, Since: at}, "", false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if diff := cmp.Diff(tc.want, tc.filter.Match(Event{Kind: KindTrialEnd, Time: at, Raw: []byte(tc.raw)})); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestFilterRejectsOpaqueIncompatibleEvidence(t *testing.T) {
	t.Parallel()
	event, err := decode([]byte(`{"seq":1,"kind":"future.additive","core":7,"trial":"one","members":"incompatible"}`))
	if err != nil {
		t.Fatalf("valid opaque event was rejected by the reader: %v", err)
	}
	if (Filter{Core: new(7), Trial: "one"}).Match(event) {
		t.Fatal("otherwise matching opaque event with incompatible evidence matched the filter")
	}
}

func TestKindSelectorValidation(t *testing.T) {
	for _, name := range []string{"trial", "trial.end", "session", "failure.carried"} {
		if err := ValidateKindSelector(name); err != nil {
			t.Fatalf("valid selector %q: %v", name, err)
		}
	}
	for _, name := range []string{"", "future", "trial.unknown", "trial.end.extra"} {
		err := ValidateKindSelector(name)
		if err == nil {
			t.Fatalf("invalid selector %q accepted", name)
		}
		prefix, valid, ok := strings.Cut(err.Error(), "; valid names: ")
		if !ok || !strings.Contains(prefix, map[bool]string{true: "empty kind list", false: "unknown kind or group"}[name == ""]) {
			t.Fatalf("selector diagnostic: %v", err)
		}
		names := strings.Split(valid, ", ")
		for i, entry := range names {
			if i > 0 && names[i-1] >= entry {
				t.Fatalf("valid names not sorted and unique: %q", valid)
			}
		}
		for _, required := range []string{"trial", "trial.end", "session.start"} {
			if !strings.Contains(", "+valid+", ", ", "+required+", ") {
				t.Fatalf("valid selector %q omitted from diagnostic", required)
			}
		}
	}
}

func TestFilterCoreSelectsTrialOutcomes(t *testing.T) {
	t.Parallel()
	at := time.Date(2026, 10, 1, 12, 0, 0, 0, time.UTC)
	lines := []string{
		`{"seq":1,"time":"2026-10-01T12:00:00Z","kind":"session.start","msg":"session"}`,
		`{"seq":2,"time":"2026-10-01T12:00:01Z","kind":"trial.intent","msg":"alone core 3","trial":"0001","core":3,"profile":[0],"regime":"R1","workload":"w","duration_s":1,"condition":"alone"}`,
		`{"seq":3,"time":"2026-10-01T12:00:02Z","kind":"trial.end","msg":"end 0001","trial":"0001","outcome":"pass","duration_s":1}`,
		`{"seq":4,"time":"2026-10-01T12:00:03Z","kind":"trial.intent","msg":"together 2-3","trial":"0002","cores":[2,3],"profile":[0],"regime":"R7","workload":"w","duration_s":1,"condition":"together"}`,
		`{"seq":5,"time":"2026-10-01T12:00:04Z","kind":"trial.end","msg":"end 0002","trial":"0002","outcome":"failure","duration_s":1}`,
		`{"seq":6,"time":"2026-10-01T12:00:05Z","kind":"failure","msg":"unattributed","trial":"0002","signal":"crash","attribution":"unattributed"}`,
		`{"seq":7,"time":"2026-10-01T12:00:06Z","kind":"failure","msg":"core 2 named","trial":"0002","core":2,"signal":"crash","attribution":"core"}`,
		`{"seq":8,"time":"2026-10-01T12:00:07Z","kind":"trial.intent","msg":"alone core 5","trial":"0003","core":5,"profile":[0],"regime":"R1","workload":"w","duration_s":1,"condition":"alone"}`,
		`{"seq":9,"time":"2026-10-01T12:00:08Z","kind":"trial.end","msg":"end 0003","trial":"0003","outcome":"failure","duration_s":1}`,
		`{"seq":10,"time":"2026-10-01T12:00:09Z","kind":"failure","msg":"unattributed 0003","trial":"0003","signal":"crash","attribution":"unattributed"}`,
		`{"seq":11,"time":"2026-10-01T12:00:10Z","kind":"session.shutdown","msg":"stop"}`,
	}
	events := make([]Event, len(lines))
	for i, line := range lines {
		e, err := decode([]byte(line))
		if err != nil {
			t.Fatalf("line %d: %v", i+1, err)
		}
		events[i] = e
	}
	seqs := func(f Filter) []int {
		var got []int
		for _, e := range f.Select(events) {
			got = append(got, e.Seq)
		}
		return got
	}
	for _, tc := range []struct {
		name   string
		filter Filter
		want   []int
	}{
		{"direct core events and the trial.end of a trial loading it", Filter{Core: new(3)}, []int{2, 3, 4, 5, 6}},
		{"a trial loading the core through cores", Filter{Core: new(2)}, []int{4, 5, 6, 7}},
		{"a trial not loading the core is excluded", Filter{Core: new(3), Trial: "0003"}, nil},
		{"session events never match", Filter{Core: new(9)}, nil},
		{"a trial that started before since still counts", Filter{Core: new(3), Since: at.Add(4 * time.Second)}, []int{5, 6}},
		{"kinds narrow the outcomes", Filter{Core: new(3), Kinds: []string{"trial.end"}}, []int{3, 5}},
		{"without a core the failure is not widened", Filter{Trial: "0002", Kinds: []string{"failure"}}, []int{6, 7}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if diff := cmp.Diff(tc.want, seqs(tc.filter)); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}
