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
		{"malformed filtered evidence", Filter{Core: new(7)}, "{", false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if diff := cmp.Diff(tc.want, tc.filter.Match(Event{Kind: KindTrialEnd, Time: at, Raw: []byte(tc.raw)})); diff != "" {
				t.Fatal(diff)
			}
		})
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
