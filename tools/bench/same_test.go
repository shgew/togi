package main

import (
	"bytes"
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"sync"
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
	if _, err := firstJournalDifference(strings.NewReader("{\"kind\":\"session.start\"\n"), strings.NewReader("{}\n")); err == nil {
		t.Fatal("malformed build-stamp event must be an error")
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
	launch := func(_ context.Context, side int, spec runSpec) (simulation, error) {
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
	different, err := runSame(context.Background(), &report, pairRuns(runs), sameConfig{jobs: 2, keep: keep, keepGoing: true}, launch)
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
			launch := func(_ context.Context, side int, spec runSpec) (simulation, error) {
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
			if different, err := runSame(context.Background(), &report, pairRuns(runs), sameConfig{jobs: 1, keep: keep}, launch); err != nil || different {
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
			dir := filepath.Join(root, sameSides[failedSide], failed.String())
			if got, err := os.ReadFile(filepath.Join(dir, "sim.log")); err != nil || string(got) != "diagnostic log\n" {
				t.Errorf("failed-run diagnostic %s: got %q, err %v", dir, got, err)
			}
			for _, side := range sameSides {
				if _, err := os.Stat(filepath.Join(root, side, matches.String())); !errors.Is(err, os.ErrNotExist) {
					t.Errorf("matching pair retained after launch failure: %s (err %v)", side, err)
				}
			}
			if _, err := os.Stat(filepath.Join(root, sameSides[1-failedSide], failed.String())); !errors.Is(err, os.ErrNotExist) {
				t.Errorf("unfinished pair's other run retained: %v", err)
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

type recordingLauncher struct {
	root    string
	journal func(side int, key sessionKey) string
	started func()

	mu       sync.Mutex
	launches []string
}

func (l *recordingLauncher) launch(ctx context.Context, side int, spec runSpec) (simulation, error) {
	key := sessionKey{spec.scenario.Name, spec.split, spec.seed}
	l.mu.Lock()
	l.launches = append(l.launches, sameSides[side]+" "+key.String())
	l.mu.Unlock()
	dir := filepath.Join(l.root, sameSides[side], key.String())
	if err := os.MkdirAll(dir, 0755); err != nil {
		return simulation{}, err
	}
	if l.started != nil {
		l.started()
	}
	if ctx.Err() != nil {
		return simulation{dir: dir}, ctx.Err()
	}
	if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte(l.journal(side, key)), 0600); err != nil {
		return simulation{}, err
	}
	return simulation{dir: dir, wall: 1}, nil
}

func samePairs(seeds ...uint64) []samePair {
	var runs [2][]runSpec
	for side := range runs {
		for _, seed := range seeds {
			runs[side] = append(runs[side], runSpec{scenario: scenario{Name: "s"}, split: "dev", seed: seed})
		}
	}
	return pairRuns(runs)
}

func filesUnder(t *testing.T, root string) []string {
	t.Helper()
	var files []string
	err := filepath.WalkDir(root, func(path string, entry os.DirEntry, err error) error {
		if err == nil && !entry.IsDir() {
			rel, _ := filepath.Rel(root, path)
			files = append(files, filepath.ToSlash(rel))
		}
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	return files
}

func TestJournalDigest(t *testing.T) {
	stamp := func(version string) string {
		return `{"kind":"session.start","session":"s","version":"` + version + `","msg":"session s started by togi ` + version + ` (schema 2)"}` + "\n"
	}
	digest := func(files map[string]string) string {
		t.Helper()
		dir := t.TempDir()
		for name, content := range files {
			path := filepath.Join(dir, name)
			if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(path, []byte(content), 0600); err != nil {
				t.Fatal(err)
			}
		}
		got, err := journalDigest(dir)
		if err != nil {
			t.Fatal(err)
		}
		return got
	}
	base := digest(map[string]string{"events.jsonl": stamp("old") + "{\"seq\":2}\n"})
	for _, tc := range []struct {
		name  string
		files map[string]string
		equal bool
	}{
		{"build stamp is ignored", map[string]string{"events.jsonl": stamp("new") + "{\"seq\":2}\n"}, true},
		{"event content", map[string]string{"events.jsonl": stamp("old") + "{\"seq\":3}\n"}, false},
		{"missing final newline", map[string]string{"events.jsonl": stamp("old") + "{\"seq\":2}"}, false},
		{"extra archive", map[string]string{"events.jsonl": stamp("old") + "{\"seq\":2}\n", "archive/a.jsonl": "{}\n"}, false},
		{"samples are not journals", map[string]string{"events.jsonl": stamp("old") + "{\"seq\":2}\n", "trials/t/samples.jsonl": "{}\n"}, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := digest(tc.files) == base; got != tc.equal {
				t.Fatalf("digest equal = %v, want %v", got, tc.equal)
			}
		})
	}
	archived := digest(map[string]string{"archive/a.jsonl": "{}\n"})
	renamed := digest(map[string]string{"archive/b.jsonl": "{}\n"})
	if archived == renamed {
		t.Fatal("journal names are part of the digest")
	}
}

func openTestCaches(t *testing.T) ([2]*sessionCache, *costTable) {
	t.Helper()
	root := t.TempDir()
	var caches [2]*sessionCache
	for side := range caches {
		var err error
		if caches[side], err = openSessionCache(root, sameSides[side]); err != nil {
			t.Fatal(err)
		}
	}
	return caches, loadCosts(filepath.Join(root, "costs.json"))
}

func TestSameCachesBothSides(t *testing.T) {
	caches, costs := openTestCaches(t)
	root := t.TempDir()
	l := &recordingLauncher{root: root, journal: func(int, sessionKey) string { return "{}\n" }}
	cfg := sameConfig{jobs: 2, caches: caches, costs: costs}
	for run, want := range []int{6, 6} {
		var report bytes.Buffer
		different, err := runSame(context.Background(), &report, samePairs(1, 2, 3), cfg, l.launch)
		if err != nil || different {
			t.Fatalf("run %d: different=%v, err=%v", run, different, err)
		}
		if len(l.launches) != want {
			t.Fatalf("run %d: %d launches in total, want %d", run, len(l.launches), want)
		}
	}
	if _, ok := costs.get("", sessionKey{"s", "dev", 2}); !ok {
		t.Error("session cost not recorded")
	}
	if files := filesUnder(t, root); len(files) != 0 {
		t.Errorf("run directories left behind: %v", files)
	}
	cfg.fresh = true
	if _, err := runSame(context.Background(), &bytes.Buffer{}, samePairs(1, 2, 3), cfg, l.launch); err != nil || len(l.launches) != 12 {
		t.Fatalf("fresh run: %d launches in total, err %v, want 12", len(l.launches), err)
	}
}

func TestSameRerunsOnlyTheSideItNeedsToShowADifference(t *testing.T) {
	caches, costs := openTestCaches(t)
	l := &recordingLauncher{root: t.TempDir(), journal: func(side int, _ sessionKey) string {
		return "{}\n{\"side\":" + string(rune('0'+side)) + "}\n"
	}}
	var report bytes.Buffer
	cfg := sameConfig{jobs: 2, caches: caches, costs: costs}
	if different, err := runSame(context.Background(), &report, samePairs(1), cfg, l.launch); err != nil || !different {
		t.Fatalf("cold run: different=%v, err=%v", different, err)
	}
	want := "same: s/dev-1\n  events.jsonl:2\n  base: {\"side\":0}\n  head: {\"side\":1}\n"
	if !strings.HasPrefix(report.String(), want) {
		t.Fatalf("report:\n%s\nwant prefix:\n%s", report.String(), want)
	}
	if len(l.launches) != 2 {
		t.Fatalf("cold run launches: %v", l.launches)
	}
	l.launches = nil
	report.Reset()
	if different, err := runSame(context.Background(), &report, samePairs(1), cfg, l.launch); err != nil || !different || !strings.HasPrefix(report.String(), want) {
		t.Fatalf("cached run: different=%v, err=%v, report:\n%s", different, err, report.String())
	}
	if len(l.launches) != 2 {
		t.Fatalf("both cached sides differ, so both must rerun to show the line: %v", l.launches)
	}
	l.launches = nil
	cfg.caches[1] = nil
	if different, err := runSame(context.Background(), &bytes.Buffer{}, samePairs(1), cfg, l.launch); err != nil || !different {
		t.Fatalf("head-only run: different=%v, err=%v", different, err)
	}
	if diff := cmp.Diff([]string{"head s/dev-1", "base s/dev-1"}, l.launches); diff != "" {
		t.Fatalf("a fresh head runs once and the cached base once more to show the line (-want +got):\n%s", diff)
	}
}

func TestSameStopsAtTheFirstDifference(t *testing.T) {
	journal := func(side int, key sessionKey) string {
		if key.seed == 1 && side == 1 {
			return "{\"changed\":true}\n"
		}
		return "{}\n"
	}
	for _, keepGoing := range []bool{false, true} {
		t.Run(map[bool]string{false: "fail fast", true: "keep going"}[keepGoing], func(t *testing.T) {
			root := t.TempDir()
			l := &recordingLauncher{root: root, journal: journal}
			var report bytes.Buffer
			different, err := runSame(context.Background(), &report, samePairs(1, 2, 3, 4), sameConfig{jobs: 1, keepGoing: keepGoing}, l.launch)
			if err != nil || !different {
				t.Fatalf("different=%v, err=%v", different, err)
			}
			wantLaunches, wantSummary := 2, "stopped at the first difference"
			if keepGoing {
				wantLaunches, wantSummary = 8, "1 of 4 sessions differ"
			}
			if len(l.launches) != wantLaunches || !strings.Contains(report.String(), wantSummary) {
				t.Fatalf("%d launches, want %d; report:\n%s", len(l.launches), wantLaunches, report.String())
			}
			want := []string{"base/s/dev-1/events.jsonl", "head/s/dev-1/events.jsonl"}
			if diff := cmp.Diff(want, filesUnder(t, root)); diff != "" {
				t.Fatalf("only the differing session keeps its runs (-want +got):\n%s", diff)
			}
		})
	}
}

func TestSameInterruptRemovesRuns(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	root := t.TempDir()
	l := &recordingLauncher{root: root, journal: func(int, sessionKey) string { return "{}\n" }, started: cancel}
	var report bytes.Buffer
	different, err := runSame(ctx, &report, samePairs(1, 2, 3, 4), sameConfig{jobs: 2}, l.launch)
	if err != nil || different || report.Len() != 0 {
		t.Fatalf("different=%v, err=%v, report %q: an interrupted run reports nothing", different, err, report.String())
	}
	if files := filesUnder(t, root); len(files) != 0 {
		t.Errorf("interrupted runs left files: %v", files)
	}
	if len(l.launches) > 2 {
		t.Errorf("runs kept starting after the interrupt: %v", l.launches)
	}
}

func TestSameReportsCPUTimeWithoutFailing(t *testing.T) {
	caches, costs := openTestCaches(t)
	pairs := samePairs(1, 2, 3)
	cpu := map[[2]uint64]float64{{0, 1}: 10, {1, 1}: 21, {0, 2}: 10, {1, 2}: 20, {0, 3}: 1, {1, 3}: 5}
	launch := func(_ context.Context, side int, spec runSpec) (simulation, error) {
		dir := filepath.Join(t.TempDir(), "run")
		if err := os.MkdirAll(dir, 0755); err != nil {
			return simulation{}, err
		}
		if err := os.WriteFile(filepath.Join(dir, "events.jsonl"), []byte("{}\n"), 0600); err != nil {
			return simulation{}, err
		}
		return simulation{dir: dir, wall: 1, cpu: cpu[[2]uint64{uint64(side), spec.seed}]}, nil
	}
	cfg := sameConfig{jobs: 2, caches: caches, costs: costs}
	want := "same: CPU time of the compared sessions: base 21.0s, head 46.0s\n" +
		"same: 2 sessions took more than 2x their base's CPU time (informational)\n" +
		"  s/dev-3: head 5.0s, base 1.0s, 5.0x\n" +
		"  s/dev-1: head 21.0s, base 10.0s, 2.1x\n" +
		"same: 0 of 3 sessions differ\n"
	for _, run := range []string{"ran", "cached"} {
		var report bytes.Buffer
		if different, err := runSame(context.Background(), &report, pairs, cfg, launch); err != nil || different {
			t.Fatalf("%s: different=%v, err=%v", run, different, err)
		}
		if diff := cmp.Diff(want, report.String()); diff != "" {
			t.Fatalf("%s report (-want +got):\n%s", run, diff)
		}
	}
}
