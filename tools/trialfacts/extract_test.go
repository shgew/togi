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
		{Session: "20260101T000000Z", Build: journal.Build{Version: "fixture", Rev: "abc", Schema: 1, Ruleset: 1}, Ruleset: 1, Seq: 3, Trial: "1", Kind: facts.TrialFact, Class: facts.Class{Regime: machine.R1, Workload: "fixture", Cores: []int{0}, DurationS: 90}, Condition: machine.Alone, Phase: journal.PhaseSearch, Profile: []int{-10, 0}, Outcome: journal.OutcomePass, DurationS: 90},
		{Session: "20260102T000000Z", Build: journal.Build{Version: "fixture", Rev: "abc", Schema: 1, Ruleset: 6}, Ruleset: 6, Seq: 3, Trial: "1", Kind: facts.TrialFact, Class: facts.Class{Regime: machine.R1, Workload: "fixture", Cores: []int{0}, DurationS: 90}, Condition: machine.Alone, Phase: journal.PhaseSearch, Profile: []int{-10, 0}, Outcome: journal.OutcomePass, DurationS: 90},
	}
	if diff := cmp.Diff(want, records); diff != "" {
		t.Fatalf("extract (-want +got):\n%s", diff)
	}
}

func TestExtractRecordOnlyPartialFacts(t *testing.T) {
	dir := t.TempDir()
	text := `{"seq":1,"kind":"session.start","session":"partial-session","schema":2,"ruleset":8,"cores":[{"core":0},{"core":1}],"msg":"private-host"}
{"seq":2,"kind":"trial.intent","trial":"pass","cores":[0],"profile":[-30,-50],"regime":"R7","workload":"fixture","duration_s":120,"condition":"resident","phase":"guard","rotation":2,"step":4,"record_only":true,"boot":"private-boot","msg":"private-host"}
{"seq":3,"kind":"trial.end","trial":"pass","outcome":"pass","duration_s":120}
{"seq":4,"kind":"trial.intent","trial":"failure","cores":[0],"profile":[-30,-50],"regime":"R7","workload":"fixture","duration_s":120,"condition":"resident","phase":"guard","rotation":2,"step":4,"record_only":true,"boot":"private-boot"}
{"seq":5,"kind":"trial.end","trial":"failure","outcome":"failure","signal":"crash","core":0,"duration_s":7,"stalled_core":0,"voltage_requests_v":{"0":1.125},"top_requesters":[0],"ccd_mhz":{"0":4800}}
{"seq":6,"kind":"trial.intent","trial":"full","cores":[0,1],"profile":[-30,-50],"regime":"R7","workload":"fixture","duration_s":120,"condition":"resident","phase":"guard"}
{"seq":7,"kind":"trial.end","trial":"full","outcome":"pass","duration_s":120}
`
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte(text), 0600); err != nil {
		t.Fatal(err)
	}
	var out bytes.Buffer
	count, err := Extract(dir, &out)
	if err != nil {
		t.Fatal(err)
	}
	if count != 3 {
		t.Fatalf("extracted facts = %d, want 3", count)
	}
	path := filepath.Join(dir, "facts.jsonl.gz")
	if err := os.WriteFile(path, out.Bytes(), 0600); err != nil {
		t.Fatal(err)
	}
	records, err := Read(path)
	if err != nil {
		t.Fatal(err)
	}
	build := journal.Build{Schema: 2, Ruleset: 8}
	class := facts.Class{Regime: machine.R7, Workload: "fixture", Cores: []int{0}, DurationS: 120}
	want := []Record{
		{Session: "partial-session", Build: build, Ruleset: 8, Seq: 3, Trial: "pass", Kind: facts.TrialFact, Class: class, Condition: machine.Together, Phase: journal.PhaseChecking, RecordOnly: true, Profile: []int{-30, -50}, Outcome: journal.OutcomePass, DurationS: 120},
		{Session: "partial-session", Build: build, Ruleset: 8, Seq: 5, Trial: "failure", Kind: facts.TrialFact, Class: class, Condition: machine.Together, Phase: journal.PhaseChecking, RecordOnly: true, Profile: []int{-30, -50}, Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 7, StalledCore: new(0), VoltageRequestsV: map[int]float64{0: 1.125}, TopRequesters: []int{0}, CCDMHz: map[int]int{0: 4800}},
		{Session: "partial-session", Build: build, Ruleset: 8, Seq: 7, Trial: "full", Kind: facts.TrialFact, Class: facts.Class{Regime: machine.R7, Workload: "fixture", Cores: []int{0, 1}, DurationS: 120}, Condition: machine.Together, Phase: journal.PhaseChecking, Profile: []int{-30, -50}, Outcome: journal.OutcomePass, DurationS: 120},
	}
	if diff := cmp.Diff(want, records); diff != "" {
		t.Fatalf("privacy-safe partial records (-want +got):\n%s", diff)
	}
	gz, err := gzip.NewReader(bytes.NewReader(out.Bytes()))
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
	if bytes.Count(plain, []byte(`"record_only":true`)) != 2 || bytes.Contains(plain, []byte(`"record_only":false`)) {
		t.Fatalf("record-only JSON marker was not retained selectively: %s", plain)
	}
	for _, private := range []string{"private-boot", "private-host", dir} {
		if bytes.Contains(plain, []byte(private)) {
			t.Fatalf("private journal metadata leaked: %q", private)
		}
	}
}
