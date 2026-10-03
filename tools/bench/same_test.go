package main

import (
	"bytes"
	"errors"
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
		{"leading object whitespace", " {\"kind\":\"session.start\"}\n", " { \"kind\":\"session.start\"}\n", &journalDifference{line: 1, base: ` {"kind":"session.start"}`, head: ` { "kind":"session.start"}`}},
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
			for _, dir := range dirs {
				if err := os.MkdirAll(filepath.Join(dir, "archive"), 0755); err != nil {
					t.Fatal(err)
				}
				if err := os.WriteFile(filepath.Join(dir, "archive", "common.jsonl"), []byte("{}\n"), 0600); err != nil {
					t.Fatal(err)
				}
			}
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

type fakeSession struct {
	key       sessionKey
	journal   string
	noJournal bool
	exit      int
	launchErr error
}

func runFakeSame(t *testing.T, root string, sessions [2][]fakeSession, keep bool) (string, bool, error) {
	t.Helper()
	var runs [2][]runSpec
	byKey := [2]map[sessionKey]fakeSession{{}, {}}
	for side := range sessions {
		for _, s := range sessions[side] {
			runs[side] = append(runs[side], runSpec{scenario: scenario{Name: s.key.scenario}, split: s.key.split, seed: s.key.seed})
			byKey[side][s.key] = s
		}
	}
	launch := func(side int, spec runSpec) (simulation, error) {
		s := byKey[side][sessionKey{spec.scenario.Name, spec.split, spec.seed}]
		dir := filepath.Join(root, sameSides[side], s.key.String())
		if err := os.MkdirAll(dir, 0755); err != nil {
			return simulation{}, err
		}
		if err := os.WriteFile(filepath.Join(dir, "sim.log"), []byte("diagnostic log\n"), 0600); err != nil {
			return simulation{}, err
		}
		if s.launchErr != nil {
			return simulation{}, s.launchErr
		}
		if !s.noJournal {
			if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte(s.journal), 0600); err != nil {
				return simulation{}, err
			}
		}
		return simulation{dir: dir, exit: s.exit}, nil
	}
	var report bytes.Buffer
	different, err := runSame(&report, pairRuns(runs), 2, keep, launch)
	return report.String(), different, err
}

func TestSameReport(t *testing.T) {
	key := sessionKey{"scenario", "dev", 1}
	got, different, err := runFakeSame(t, t.TempDir(), [2][]fakeSession{
		{{key: key, journal: "{}\n{\"decision\":0}\n"}, {key: sessionKey{"removed", "holdout", 2}, journal: "{}\n"}},
		{{key: key, journal: "{}\n{\"decision\":1}\n", exit: 1}, {key: sessionKey{"added", "holdout", 3}, journal: "{}\n"}},
	}, false)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(true, different); diff != "" {
		t.Fatal(diff)
	}
	path := filepath.Join("testdata", "same.golden")
	if *update {
		if err := os.WriteFile(path, []byte(got), 0600); err != nil {
			t.Fatal(err)
		}
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(string(want), got); diff != "" {
		t.Fatal(diff)
	}
}

func TestSameExitCodesAndSplits(t *testing.T) {
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
			_, got, err := runFakeSame(t, t.TempDir(), [2][]fakeSession{
				{{key: sessionKey{"s", tc.baseSplit, 1}, journal: "{}\n", exit: tc.baseExit}},
				{{key: sessionKey{"s", tc.headSplit, 1}, journal: "{}\n", exit: tc.headExit}},
			}, false)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.wantDifference, got); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestSameKeepsOnlyDifferingRuns(t *testing.T) {
	matches, differs := sessionKey{"matches", "dev", 1}, sessionKey{"differs", "dev", 1}
	for _, tc := range []struct {
		name string
		keep bool
		want []string
	}{
		{"matching runs removed", false, []string{"base/differs/dev-1", "head/differs/dev-1"}},
		{"keep retains every run", true, []string{"base/differs/dev-1", "base/matches/dev-1", "head/differs/dev-1", "head/matches/dev-1"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			root := t.TempDir()
			if _, _, err := runFakeSame(t, root, [2][]fakeSession{
				{{key: matches, journal: "{}\n"}, {key: differs, journal: "{}\n"}},
				{{key: matches, journal: "{}\n"}, {key: differs, journal: "{}\n", exit: 1}},
			}, tc.keep); err != nil {
				t.Fatal(err)
			}
			var got []string
			err := filepath.WalkDir(root, func(path string, entry os.DirEntry, err error) error {
				if err != nil || entry.Name() != "events.jsonl" {
					return err
				}
				rel, err := filepath.Rel(root, filepath.Dir(path))
				got = append(got, filepath.ToSlash(rel))
				return err
			})
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestSameRemovesMatchingRunsBeforeNextPair(t *testing.T) {
	for _, keep := range []bool{false, true} {
		t.Run(map[bool]string{false: "remove", true: "keep"}[keep], func(t *testing.T) {
			root := t.TempDir()
			keys := []sessionKey{{"first", "dev", 1}, {"second", "dev", 1}}
			var runs [2][]runSpec
			for side := range runs {
				for _, key := range keys {
					runs[side] = append(runs[side], runSpec{scenario: scenario{Name: key.scenario}, split: key.split, seed: key.seed})
				}
			}
			launch := func(side int, spec runSpec) (simulation, error) {
				if side == 0 && spec.scenario.Name == "second" {
					for _, name := range sameSides {
						dir := filepath.Join(root, name, keys[0].String())
						_, err := os.Stat(dir)
						if keep && err != nil {
							t.Errorf("kept matching directory %s: %v", dir, err)
						}
						if !keep && !errors.Is(err, os.ErrNotExist) {
							t.Errorf("matching directory still present before next pair: %s (err %v)", dir, err)
						}
					}
				}
				key := sessionKey{spec.scenario.Name, spec.split, spec.seed}
				dir := filepath.Join(root, sameSides[side], key.String())
				if err := os.MkdirAll(filepath.Join(dir, "samples"), 0755); err != nil {
					return simulation{}, err
				}
				for _, file := range []string{"events.jsonl", "sim.log", "samples/trial.json"} {
					if err := os.WriteFile(filepath.Join(dir, file), []byte("{}\n"), 0600); err != nil {
						return simulation{}, err
					}
				}
				return simulation{dir: dir}, nil
			}
			var report bytes.Buffer
			if different, err := runSame(&report, pairRuns(runs), 1, keep, launch); err != nil || different {
				t.Fatalf("matching pairs: different=%v, err=%v", different, err)
			}
			for _, name := range sameSides {
				for _, key := range keys {
					dir := filepath.Join(root, name, key.String())
					if !keep {
						if _, err := os.Stat(dir); !errors.Is(err, os.ErrNotExist) {
							t.Errorf("matching directory retained: %s (err %v)", dir, err)
						}
						continue
					}
					for _, file := range []string{"events.jsonl", "sim.log", "samples/trial.json"} {
						if _, err := os.Stat(filepath.Join(dir, file)); err != nil {
							t.Errorf("kept artifact %s/%s: %v", dir, file, err)
						}
					}
				}
			}
		})
	}
}

func TestSameKeepsFailedPairs(t *testing.T) {
	for failedSide, name := range sameSides {
		t.Run(name, func(t *testing.T) {
			root := t.TempDir()
			failed, matches := sessionKey{"failed", "dev", 1}, sessionKey{"matches", "dev", 1}
			failure := errors.New("launcher failed")
			sessions := [2][]fakeSession{
				{{key: failed, journal: "{}\n"}, {key: matches, journal: "{}\n"}},
				{{key: failed, journal: "{}\n"}, {key: matches, journal: "{}\n"}},
			}
			sessions[failedSide][0].launchErr = failure
			report, _, err := runFakeSame(t, root, sessions, false)
			if !errors.Is(err, failure) || !strings.Contains(err.Error(), name+" "+failed.String()) {
				t.Fatalf("want contextual launch error, got %v", err)
			}
			if report != "" {
				t.Fatalf("failed run must not report equality: %q", report)
			}
			for _, side := range sameSides {
				dir := filepath.Join(root, side, failed.String())
				if got, err := os.ReadFile(filepath.Join(dir, "sim.log")); err != nil || string(got) != "diagnostic log\n" {
					t.Errorf("failed-pair diagnostic %s: got %q, err %v", dir, got, err)
				}
				if _, err := os.Stat(filepath.Join(root, side, matches.String())); !errors.Is(err, os.ErrNotExist) {
					t.Errorf("matching pair retained after launch failure: %s (err %v)", side, err)
				}
			}
		})
	}
}

func TestSameRejectsMissingJournals(t *testing.T) {
	key := sessionKey{"s", "dev", 1}
	report, _, err := runFakeSame(t, t.TempDir(), [2][]fakeSession{
		{{key: key, noJournal: true, exit: 1}},
		{{key: key, noJournal: true, exit: 1}},
	}, false)
	if err == nil || !strings.Contains(err.Error(), "missing journals") {
		t.Fatalf("want missing-journal execution error, got %v", err)
	}
	if strings.Contains(report, "0 of 1 sessions differ") {
		t.Fatal("failed simulations must not report equality")
	}
}

func TestSameRejectsEmptyJournals(t *testing.T) {
	key := sessionKey{"s", "dev", 1}
	report, _, err := runFakeSame(t, t.TempDir(), [2][]fakeSession{
		{{key: key, exit: 1}},
		{{key: key, exit: 1}},
	}, false)
	if err == nil || !strings.Contains(err.Error(), "journals contain no events") {
		t.Fatalf("want empty-journal execution error, got %v", err)
	}
	if strings.Contains(report, "0 of 1 sessions differ") {
		t.Fatal("zero-event simulations must not report equality")
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
