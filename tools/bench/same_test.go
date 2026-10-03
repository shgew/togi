package main

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestNormalizeBuild(t *testing.T) {
	for _, tc := range []struct {
		name, input, want string
	}{
		{
			"session start",
			`{"kind":"session.start","msg":"session s started by togi old+abc (schema 2, ruleset 7, fixes 3, 2 cores); togi old+abc","version":"old","rev":"abc","session":"s","ruleset":7,"schema":2,"fixes":3,"evidence_epoch":2,"detail":"togi old+abc","source":{"version":"source","rev":"def"}}`,
			`{"kind":"session.start","msg":"session s started by  (schema 2, ruleset 7, fixes 3, 2 cores); togi old+abc","session":"s","ruleset":7,"schema":2,"fixes":3,"evidence_epoch":2,"detail":"togi old+abc","source":{"version":"source","rev":"def"}}`,
		},
		{
			"config file",
			`{"kind":"config.loaded","version":"old","rev":"","path":"config.toml","file":true,"msg":"config loaded from config.toml by togi old (schema 2, ruleset 7, fixes 3)"}`,
			`{"kind":"config.loaded","path":"config.toml","file":true,"msg":"config loaded from config.toml by  (schema 2, ruleset 7, fixes 3)"}`,
		},
		{
			"config defaults",
			`{"kind":"config.loaded","path":"config.toml","file":false,"msg":"no config at config.toml; using defaults with a togi build from before version stamps (schema 2, ruleset 7, fixes 3)","version":"","rev":""}`,
			`{"kind":"config.loaded","path":"config.toml","file":false,"msg":"no config at config.toml; using defaults with  (schema 2, ruleset 7, fixes 3)"}`,
		},
		{
			"leading build fields",
			`{"version":"old", "rev":"abc", "kind":"session.start","session":"s","msg":"session s started by togi old+abc (schema 2)"}`,
			`{"kind":"session.start","session":"s","msg":"session s started by  (schema 2)"}`,
		},
		{
			"unrelated event",
			` {"kind":"trial.end", "version":1,"rev":"abc","msg":"togi old+abc"} `,
			` {"kind":"trial.end", "version":1,"rev":"abc","msg":"togi old+abc"} `,
		},
		{
			"wrong message location",
			`{"kind":"session.start","session":"s","version":"old","rev":"abc","msg":"unexpected togi old+abc"}`,
			`{"kind":"session.start","session":"s","msg":"unexpected togi old+abc"}`,
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got, err := normalizeBuild([]byte(tc.input + "\n"))
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.want+"\n", string(got)); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestBuildComparisonPreservesDecisions(t *testing.T) {
	base := `{"kind":"session.start","session":"s","version":"old","rev":"abc","msg":"session s started by togi old+abc (schema 2, ruleset 7, fixes 3, 2 cores)","schema":2,"ruleset":7,"fixes":3,"evidence_epoch":2,"evidence":2,"cores":[]}` + "\n"
	head := strings.ReplaceAll(strings.ReplaceAll(base, "old", "new"), "abc", "def")
	for _, tc := range []struct {
		name, before, after string
		wantDifference      bool
	}{
		{name: "build only"},
		{"non-build field", `"session":"s"`, `"session":"other"`, true},
		{"ruleset", `"ruleset":7`, `"ruleset":8`, true},
		{"schema", `"schema":2`, `"schema":3`, true},
		{"fixes", `"fixes":3`, `"fixes":4`, true},
		{"evidence epoch", `"evidence_epoch":2`, `"evidence_epoch":3`, true},
		{"session evidence", `"evidence":2`, `"evidence":3`, true},
		{"message decision", "2 cores", "3 cores", true},
		{"byte formatting", `"cores":[]`, `"cores": []`, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			candidate := head
			if tc.before != "" {
				candidate = strings.Replace(candidate, tc.before, tc.after, 1)
			}
			diff, err := firstJournalDifference(strings.NewReader(base), strings.NewReader(candidate))
			if err != nil {
				t.Fatal(err)
			}
			if d := cmp.Diff(tc.wantDifference, diff != nil); d != "" {
				t.Fatal(d)
			}
		})
	}
}

func TestFirstJournalDifference(t *testing.T) {
	for _, tc := range []struct {
		name, base, head string
		want             *journalDifference
	}{
		{"equal", "{}\n", "{}\n", nil},
		{"third line", "{}\n{}\n{\"seq\":3}\n{\"seq\":4}\n", "{}\n{}\n{\"seq\":9}\n{}\n", &journalDifference{line: 3, base: `{"seq":3}`, head: `{"seq":9}`}},
		{"missing event", "{}\n{}\n", "{}\n", &journalDifference{line: 2, base: "{}", head: "<missing event>"}},
		{"extra event", "{}\n", "{}\n{}\n", &journalDifference{line: 2, base: "<missing event>", head: "{}"}},
		{"missing newline", "{}\n", "{}", &journalDifference{line: 1, base: "{}", head: "{}"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got, err := firstJournalDifference(strings.NewReader(tc.base), strings.NewReader(tc.head))
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.want, got, cmp.AllowUnexported(journalDifference{})); diff != "" {
				t.Fatal(diff)
			}
		})
	}
	if _, err := firstJournalDifference(strings.NewReader("{\n"), strings.NewReader("{}\n")); err == nil {
		t.Fatal("malformed event must be an error")
	}
}

func TestCompareJournals(t *testing.T) {
	for _, tc := range []struct {
		name, file, content string
		side                int
		want                *journalDifference
	}{
		{"equal", "", "", 0, nil},
		{"head missing archive", "archive/session.jsonl", "{}\n", 0, &journalDifference{file: "archive/session.jsonl", line: 1, base: "{}", head: "<missing file>"}},
		{"base missing current", "events.jsonl", "{}\n", 1, &journalDifference{file: "events.jsonl", line: 1, base: "<missing file>", head: "{}"}},
		{"empty file still exists", "events.jsonl", "", 0, &journalDifference{file: "events.jsonl", line: 1, base: "<empty file>", head: "<missing file>"}},
		{"samples are not journals", "trials/t/samples.jsonl", "{}\n", 1, nil},
		{"archived samples are not journals", "archive/session-trials/t/samples.jsonl", "{}\n", 1, nil},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dirs := [2]string{t.TempDir(), t.TempDir()}
			if tc.file != "" {
				path := filepath.Join(dirs[tc.side], tc.file)
				if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
					t.Fatal(err)
				}
				if err := os.WriteFile(path, []byte(tc.content), 0600); err != nil {
					t.Fatal(err)
				}
			}
			got, err := compareJournals(dirs[0], dirs[1])
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.want, got, cmp.AllowUnexported(journalDifference{})); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestSameReport(t *testing.T) {
	key := sessionKey{"scenario", "dev", 1}
	base := []sameSession{{key, simulation{dir: t.TempDir(), exit: 0}}}
	head := []sameSession{{key, simulation{dir: t.TempDir(), exit: 1}}}
	for side, sessions := range [2][]sameSession{base, head} {
		text := fmt.Sprintf("{}\n{\"decision\":%d}\n", side)
		if err := os.WriteFile(filepath.Join(sessions[0].run.dir, "events.jsonl"), []byte(text), 0600); err != nil {
			t.Fatal(err)
		}
	}
	base = append(base, sameSession{key: sessionKey{"removed", "holdout", 2}})
	head = append(head, sameSession{key: sessionKey{"added", "holdout", 3}})
	var got bytes.Buffer
	different, err := compareSessions(&got, base, head)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(true, different); diff != "" {
		t.Fatal(diff)
	}
	path := filepath.Join("testdata", "same.golden")
	if *update {
		if err := os.WriteFile(path, got.Bytes(), 0600); err != nil {
			t.Fatal(err)
		}
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(string(want), got.String()); diff != "" {
		t.Fatal(diff)
	}
}

func TestSameExitCodesAndSplits(t *testing.T) {
	dirs := [2]string{t.TempDir(), t.TempDir()}
	for _, tc := range []struct {
		name, baseSplit, headSplit string
		baseExit, headExit         int
		wantDifference             bool
	}{
		{"equal nonzero exits", "dev", "dev", 1, 1, false},
		{"different exits", "dev", "dev", 0, 1, true},
		{"split is part of identity", "dev", "holdout", 0, 0, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			base := []sameSession{{sessionKey{"s", tc.baseSplit, 1}, simulation{dir: dirs[0], exit: tc.baseExit}}}
			head := []sameSession{{sessionKey{"s", tc.headSplit, 1}, simulation{dir: dirs[1], exit: tc.headExit}}}
			var report bytes.Buffer
			got, err := compareSessions(&report, base, head)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.wantDifference, got); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestSameRejectsExplicitBenchFlags(t *testing.T) {
	for _, argument := range []string{"--split=dev", "--split=all", "--baseline=", "--out="} {
		t.Run(argument, func(t *testing.T) {
			var stdout, stderr bytes.Buffer
			got := run([]string{"--same=base", argument}, &stdout, &stderr)
			if diff := cmp.Diff(2, got); diff != "" {
				t.Fatal(diff)
			}
			if diff := cmp.Diff("", stdout.String()); diff != "" {
				t.Fatal(diff)
			}
			if !strings.Contains(stderr.String(), "--same cannot be combined") {
				t.Fatalf("missing usage diagnostic: %s", stderr.String())
			}
		})
	}
}
