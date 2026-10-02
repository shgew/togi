package trialfacts

import (
	"bytes"
	"compress/gzip"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestExtractAllSessionsPrivacyAndDeterminism(t *testing.T) {
	dir := t.TempDir()
	if err := os.Mkdir(filepath.Join(dir, "archive"), 0700); err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct {
		path, session string
		ruleset       int
	}{
		{"archive/old.jsonl", "20260101T000000Z", 1},
		{"events.jsonl", "20260102T000000Z", 6},
	} {
		text := `{"seq":1,"kind":"session.start","session":"SESSION","schema":1,"ruleset":RULESET,"version":"fixture","rev":"abc","boot":"private-boot","time":"2026-01-01T12:34:56Z","msg":"private-host private-path","cores":[{"core":0},{"core":1}]}
{"seq":2,"kind":"trial.intent","trial":"1","core":0,"offset":-10,"regime":"R1","workload":"fixture","duration_s":90,"condition":"isolated","phase":"search","boot":"private-boot","time":"2026-01-01T12:34:56Z","msg":"private-host private-path"}
{"seq":3,"kind":"trial.end","trial":"1","outcome":"pass","duration_s":90,"boot":"private-boot","time":"2026-01-01T12:34:56Z","msg":"private-host private-path"}
`
		text = strings.ReplaceAll(text, "SESSION", tc.session)
		if tc.ruleset == 1 {
			text = strings.ReplaceAll(text, "RULESET", "1")
		} else {
			text = strings.ReplaceAll(text, "RULESET", "6")
		}
		if err := os.WriteFile(filepath.Join(dir, tc.path), []byte(text), 0600); err != nil {
			t.Fatal(err)
		}
	}
	var first, second bytes.Buffer
	for _, dst := range []*bytes.Buffer{&first, &second} {
		count, err := Extract(dir, dst)
		if err != nil {
			t.Fatal(err)
		}
		if count != 2 {
			t.Fatalf("facts=%d want 2", count)
		}
	}
	if diff := cmp.Diff(first.Bytes(), second.Bytes()); diff != "" {
		t.Fatalf("extract determinism (-want +got):\n%s", diff)
	}
	gz, err := gzip.NewReader(bytes.NewReader(first.Bytes()))
	if err != nil {
		t.Fatal(err)
	}
	plain, err := io.ReadAll(gz)
	if err != nil {
		t.Fatal(err)
	}
	if err := gz.Close(); err != nil {
		t.Fatal(err)
	}
	for _, private := range []string{"private-boot", "private-host", "private-path", "2026-01-01T12:34:56Z", dir} {
		if bytes.Contains(plain, []byte(private)) {
			t.Fatalf("private journal metadata leaked: %q", private)
		}
	}
	path := filepath.Join(dir, "facts.jsonl.gz")
	if err := os.WriteFile(path, first.Bytes(), 0600); err != nil {
		t.Fatal(err)
	}
	records, err := Read(path)
	if err != nil {
		t.Fatal(err)
	}
	want := []Record{
		{Session: "20260101T000000Z", Build: journal.Build{Version: "fixture", Rev: "abc", Schema: 1, Ruleset: 1}, Ruleset: 1, Seq: 3, Trial: "1", Kind: facts.TrialFact, Class: facts.Class{Regime: machine.R1, Workload: "fixture", Cores: []int{0}, DurationS: 90}, Condition: machine.Isolated, Phase: journal.PhaseSearch, Profile: []int{-10, 0}, Outcome: journal.OutcomePass, DurationS: 90},
		{Session: "20260102T000000Z", Build: journal.Build{Version: "fixture", Rev: "abc", Schema: 1, Ruleset: 6}, Ruleset: 6, Seq: 3, Trial: "1", Kind: facts.TrialFact, Class: facts.Class{Regime: machine.R1, Workload: "fixture", Cores: []int{0}, DurationS: 90}, Condition: machine.Isolated, Phase: journal.PhaseSearch, Profile: []int{-10, 0}, Outcome: journal.OutcomePass, DurationS: 90},
	}
	if diff := cmp.Diff(want, records); diff != "" {
		t.Fatalf("extract (-want +got):\n%s", diff)
	}
}
