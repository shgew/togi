package main

import (
	"bytes"
	"context"
	"errors"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/simrun"
	"github.com/shgew/togi/tools/forecast"
	"github.com/shgew/togi/tools/modelcheck"
	"github.com/shgew/togi/tools/trialfacts"
)

// launchInProcess runs tools/sim's session in the test process, with its exit statuses.
func launchInProcess(root string) func(runSpec) (simulation, error) {
	return func(spec runSpec) (simulation, error) {
		dir := runDir(root, spec)
		cfg := spec.cfg
		cfg.Seed = spec.seed
		cfg, err := sim.Resume(dir, cfg)
		if err != nil {
			return simulation{}, err
		}
		m, err := sim.New(cfg)
		if err != nil {
			return simulation{}, err
		}
		recorded, err := simrun.RecordedConfig(dir, config.Default())
		if err != nil {
			return simulation{}, err
		}
		stop, err := simrun.Simulate(context.Background(), simrun.Input{Config: recorded, ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Log: io.Discard, Cycles: 1, InMemoryJournal: true})
		exit := 0
		switch {
		case errors.Is(err, simrun.ErrBootCap):
			exit = 3
		case err != nil:
			return simulation{}, err
		case stop.Reason == session.StopDeadEnd:
			exit = 1
		}
		return simulation{dir: dir, exit: exit}, nil
	}
}

// stoppedCopy is a simulated state directory whose session stopped at the first event until accepts. With inFlight,
// its journal ends at that event, as if the machine lost power there.
func stoppedCopy(t *testing.T, until func(journal.Event, []journal.Event) bool, inFlight bool) string {
	t.Helper()
	dir := t.TempDir()
	m, err := sim.New(sim.Config{Seed: 42})
	if err != nil {
		t.Fatal(err)
	}
	var seen []journal.Event
	stopSeq := 0
	in := simrun.Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Log: io.Discard, Cycles: 1, InMemoryJournal: true,
		Until: func(e journal.Event) bool {
			seen = append(seen, e)
			if until(e, seen) {
				stopSeq = e.Seq
				return true
			}
			return false
		},
	}
	if _, err := simrun.Simulate(context.Background(), in); err != nil {
		t.Fatal(err)
	}
	if stopSeq == 0 {
		t.Fatal("the simulated session never reached its stopping event")
	}
	if inFlight {
		path := filepath.Join(dir, "events.jsonl")
		events, err := journal.ReadHistory(path)
		if err != nil {
			t.Fatal(err)
		}
		data, err := os.ReadFile(path)
		if err != nil {
			t.Fatal(err)
		}
		lines := bytes.SplitAfter(data, []byte{'\n'})
		for i, e := range events {
			if e.Seq == stopSeq {
				data = bytes.Join(lines[:i+1], nil)
			}
		}
		if err := os.WriteFile(path, data, 0o644); err != nil {
			t.Fatal(err)
		}
	}
	return dir
}

func snapshot(t *testing.T, dir string) map[string]string {
	t.Helper()
	files := map[string]string{}
	err := filepath.WalkDir(dir, func(path string, entry fs.DirEntry, err error) error {
		if err != nil || entry.IsDir() {
			return err
		}
		data, err := os.ReadFile(path)
		files[path] = string(data)
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	return files
}

func crashes(t *testing.T, dir, from string) int {
	t.Helper()
	sessions, err := forecast.ReadDir(dir, from)
	if err != nil {
		t.Fatal(err)
	}
	n := 0
	for _, s := range sessions {
		for _, e := range s.Events {
			if e.Kind == journal.KindCrashDetected {
				n++
			}
		}
	}
	return n
}

func TestForecastFromSimulatedCopy(t *testing.T) {
	afterCrash := func(e journal.Event, seen []journal.Event) bool {
		return e.Kind == journal.KindCrashDetected
	}
	trialAfterCrash := func(e journal.Event, seen []journal.Event) bool {
		if e.Kind != journal.KindTrialStart {
			return false
		}
		for _, s := range seen {
			if s.Kind == journal.KindCrashDetected {
				return true
			}
		}
		return false
	}
	for _, tc := range []struct {
		name     string
		until    func(journal.Event, []journal.Event) bool
		inFlight bool
	}{
		{"stopped", afterCrash, false},
		{"in-flight", trialAfterCrash, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			input := stoppedCopy(t, tc.until, tc.inFlight)
			before := snapshot(t, input)
			anchor, err := forecast.AnchorOf(input)
			if err != nil {
				t.Fatal(err)
			}
			if tc.inFlight {
				events, err := journal.ReadHistory(filepath.Join(input, "events.jsonl"))
				if err != nil {
					t.Fatal(err)
				}
				if last := events[len(events)-1]; last.Kind != journal.KindTrialStart || last.Seq != anchor.Seq {
					t.Fatalf("copy ends at %s %d, want the in-flight trial's start as the anchor", last.Kind, last.Seq)
				}
			}
			earlier := crashes(t, input, "")
			if earlier == 0 {
				t.Fatal("the copy has no crash before the anchor to exclude")
			}
			specs, err := ensembleRuns(filepath.Join("testdata", "forecast-suite.toml"), trialfacts.Extracts{})
			if err != nil {
				t.Fatal(err)
			}
			if len(specs) != 3 {
				t.Fatalf("ensemble has %d runs, want the target scenario's 3", len(specs))
			}
			var tables, records [2]bytes.Buffer
			for i := range 2 {
				root := t.TempDir()
				record, err := makeForecast(input, root, anchor, specs, "testdata", len(specs), true, launchInProcess(root))
				if err != nil {
					t.Fatal(err)
				}
				if err := reportForecast(&tables[i], record, "testdata", nil); err != nil {
					t.Fatal(err)
				}
				if err := record.Write(&records[i]); err != nil {
					t.Fatal(err)
				}
				for j, spec := range specs {
					if got, want := record.Runs[j].Crashes, crashes(t, runDir(root, spec), anchor.Session)-earlier; got != want {
						t.Errorf("run %d counts %d crashes after the anchor, its journal %d", spec.seed, got, want)
					}
				}
			}
			if diff := cmp.Diff(tables[0].String(), tables[1].String()); diff != "" {
				t.Errorf("tables differ between identical forecasts: %s", diff)
			}
			if diff := cmp.Diff(records[0].String(), records[1].String()); diff != "" {
				t.Errorf("records differ between identical forecasts: %s", diff)
			}
			if diff := cmp.Diff(before, snapshot(t, input)); diff != "" {
				t.Errorf("forecast changed its input: %s", diff)
			}
		})
	}
}

// TestForecastReportRendersRecordedForecasts renders forecasts captured once from simulated runs. Their build
// stamps are literal, so a tuner change leaves the goldens alone.
func TestForecastReportRendersRecordedForecasts(t *testing.T) {
	for _, name := range []string{"stopped", "in-flight"} {
		t.Run(name, func(t *testing.T) {
			record, err := forecast.ReadRecord(filepath.Join("testdata", "forecast-"+name+".json"))
			if err != nil {
				t.Fatal(err)
			}
			var table bytes.Buffer
			if err := reportForecast(&table, record, "testdata", nil); err != nil {
				t.Fatal(err)
			}
			path := filepath.Join("testdata", "forecast-"+name+".golden")
			if *update {
				if err := os.WriteFile(path, table.Bytes(), 0o600); err != nil {
					t.Fatal(err)
				}
			}
			want, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(string(want), table.String()); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestForecastFailsOnFailedRun(t *testing.T) {
	input := stoppedCopy(t, func(e journal.Event, _ []journal.Event) bool { return e.Kind == journal.KindTrialEnd }, false)
	anchor, err := forecast.AnchorOf(input)
	if err != nil {
		t.Fatal(err)
	}
	specs, err := ensembleRuns(filepath.Join("testdata", "forecast-suite.toml"), trialfacts.Extracts{})
	if err != nil {
		t.Fatal(err)
	}
	for _, run := range []simulation{{exit: 2}, {exit: -1, timedOut: true}, {exit: 0}} {
		root := t.TempDir()
		launch := func(spec runSpec) (simulation, error) { return run, nil }
		if _, err := makeForecast(input, root, anchor, specs, "testdata", 1, false, launch); err == nil {
			t.Errorf("forecast accepted a run with exit %d, timed out %v and no conclusion", run.exit, run.timedOut)
		}
	}
}

func TestForecastRejectsDeadEndBeforeAnchor(t *testing.T) {
	input := stoppedCopy(t, func(e journal.Event, _ []journal.Event) bool { return e.Kind == journal.KindTrialEnd }, false)
	j, err := journal.Open(input, journal.Options{})
	if err != nil {
		t.Fatal(err)
	}
	deadEnd, err := j.Append(&journal.DeadEnd{Condition: journal.DeadEndThermalTrip, Detail: "thermal trip", Action: journal.ActionExit})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := j.Append(&journal.Shutdown{Reason: journal.ShutdownDeadEnd}, deadEnd.Seq); err != nil {
		t.Fatal(err)
	}
	if err := j.Close(); err != nil {
		t.Fatal(err)
	}
	anchor, err := forecast.AnchorOf(input)
	if err != nil {
		t.Fatal(err)
	}
	if anchor.Seq <= deadEnd.Seq {
		t.Fatalf("anchor %d does not follow the dead end %d", anchor.Seq, deadEnd.Seq)
	}
	specs, err := ensembleRuns(filepath.Join("testdata", "forecast-suite.toml"), trialfacts.Extracts{})
	if err != nil {
		t.Fatal(err)
	}
	launch := func(spec runSpec) (simulation, error) { return simulation{exit: 1}, nil }
	if _, err := makeForecast(input, t.TempDir(), anchor, specs, "testdata", 1, false, launch); err == nil {
		t.Error("forecast accepted exit 1 as a dead end from a dead end the copy already held")
	}
}

func TestCopyStateCopiesSessionBearingArchive(t *testing.T) {
	src, dst := t.TempDir(), t.TempDir()
	files := map[string]string{
		"events.jsonl": "live\n",
		"state.json":   "{}\n",
		filepath.Join("archive", "20260101T000000Z.jsonl"):          "archived\n",
		filepath.Join("archive", "20260102T000000Z-carry-pending"):  "carry\n",
		filepath.Join("archive", "20260102T000000Z-compat-pending"): "compat\n",
		filepath.Join("archive", "20260101T000000Z-reset-all"):      "",
	}
	if err := os.Mkdir(filepath.Join(src, "archive"), 0o755); err != nil {
		t.Fatal(err)
	}
	want := map[string]string{}
	for name, data := range files {
		if err := os.WriteFile(filepath.Join(src, name), []byte(data), 0o644); err != nil {
			t.Fatal(err)
		}
		want[filepath.Join(dst, name)] = data
	}
	if err := os.WriteFile(filepath.Join(src, "archive", "notes"), []byte("not a session\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := copyState(src, dst); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(want, snapshot(t, dst)); diff != "" {
		t.Error(diff)
	}
}

func TestCopyStateRejectsIrregularArchiveEntry(t *testing.T) {
	for _, name := range []string{"20260101T000000Z.jsonl", "20260101T000000Z-carry-pending", "20260101T000000Z-compat-pending", "20260101T000000Z-reset-all"} {
		src := t.TempDir()
		if err := os.WriteFile(filepath.Join(src, "events.jsonl"), []byte("live\n"), 0o644); err != nil {
			t.Fatal(err)
		}
		if err := os.Mkdir(filepath.Join(src, "archive"), 0o755); err != nil {
			t.Fatal(err)
		}
		target := filepath.Join(t.TempDir(), "elsewhere")
		if err := os.WriteFile(target, []byte("archived\n"), 0o644); err != nil {
			t.Fatal(err)
		}
		if err := os.Symlink(target, filepath.Join(src, "archive", name)); err != nil {
			t.Fatal(err)
		}
		err := copyState(src, t.TempDir())
		if err == nil || !strings.Contains(err.Error(), name) {
			t.Errorf("%s: copy error %v, want one naming the symlinked entry", name, err)
		}
	}
}

func TestFailedForecastKeepsOnlyFailedRuns(t *testing.T) {
	input := stoppedCopy(t, func(e journal.Event, _ []journal.Event) bool { return e.Kind == journal.KindTrialEnd }, false)
	anchor, err := forecast.AnchorOf(input)
	if err != nil {
		t.Fatal(err)
	}
	specs, err := ensembleRuns(filepath.Join("testdata", "forecast-suite.toml"), trialfacts.Extracts{})
	if err != nil {
		t.Fatal(err)
	}
	exists := func(path string) bool {
		_, err := os.Stat(path)
		if err != nil && !errors.Is(err, fs.ErrNotExist) {
			t.Fatal(err)
		}
		return err == nil
	}
	t.Run("failed run", func(t *testing.T) {
		root := t.TempDir()
		inProcess := launchInProcess(root)
		launch := func(spec runSpec) (simulation, error) {
			if runDir(root, spec) == runDir(root, specs[0]) {
				return simulation{exit: 2}, nil
			}
			return inProcess(spec)
		}
		_, err := makeForecast(input, root, anchor, specs, "testdata", len(specs), false, launch)
		if err == nil {
			t.Fatal("forecast accepted a failed run")
		}
		var stderr bytes.Buffer
		finishRuns(root, false, err, &stderr)
		if diff := cmp.Diff("bench: keeping runs in "+root+"\n", stderr.String()); diff != "" {
			t.Error(diff)
		}
		for i, spec := range specs {
			if got, want := exists(runDir(root, spec)), i == 0; got != want {
				t.Errorf("run %s-%d kept %v, want %v", spec.split, spec.seed, got, want)
			}
		}
	})
	t.Run("later step", func(t *testing.T) {
		root := filepath.Join(t.TempDir(), "runs")
		record, err := makeForecast(input, root, anchor, specs, "testdata", len(specs), false, launchInProcess(root))
		if err != nil {
			t.Fatal(err)
		}
		file := filepath.Join(t.TempDir(), "file")
		if err := os.WriteFile(file, nil, 0o644); err != nil {
			t.Fatal(err)
		}
		err = writeRecord(filepath.Join(file, "forecast.json"), record)
		if err == nil {
			t.Fatal("wrote a record under a regular file")
		}
		var stderr bytes.Buffer
		finishRuns(root, false, err, &stderr)
		if stderr.Len() != 0 {
			t.Errorf("printed %q with no failed run", stderr.String())
		}
		if exists(root) {
			t.Error("kept the run root with no failed run")
		}
	})
}

func TestRelativeToResolvesMixedPaths(t *testing.T) {
	wd, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct{ dir, path string }{
		{"testdata", filepath.Join(wd, "testdata", "machines", "target.toml")},
		{filepath.Join(wd, "testdata"), filepath.Join("testdata", "machines", "target.toml")},
		{"testdata", filepath.Join("testdata", "machines", "target.toml")},
	} {
		got, err := relativeTo(tc.dir, tc.path)
		if err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff("machines/target.toml", got); diff != "" {
			t.Errorf("%s from %s: %s", tc.path, tc.dir, diff)
		}
	}
}

func TestForecastReportNamesMachinesRelativeToSuite(t *testing.T) {
	wd, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	suiteDir := filepath.Join(wd, "testdata")
	check := &modelcheck.Result{Machine: filepath.Join(suiteDir, "machines", "target.toml"), Status: "ok", Groups: []modelcheck.Group{}}
	record := forecast.Record{Commit: "0123abc", Runs: []forecast.Run{{Status: forecast.Censored}}}
	var out bytes.Buffer
	if err := reportForecast(&out, record, suiteDir, []*modelcheck.Result{check}); err != nil {
		t.Fatal(err)
	}
	if strings.Contains(out.String(), wd) {
		t.Errorf("report holds the local path %s:\n%s", wd, out.String())
	}
	if want := "\nmachines/target.toml: ok (0 eligible groups; 0 idle failures without trial exposure)\n"; !strings.Contains(out.String(), want) {
		t.Errorf("report lacks %q:\n%s", want, out.String())
	}
	if diff := cmp.Diff(filepath.Join(suiteDir, "machines", "target.toml"), check.Machine); diff != "" {
		t.Errorf("report changed the check: %s", diff)
	}
}

func TestForecastRejectsConflictingFlags(t *testing.T) {
	for _, flag := range []string{"--split=all", "--baseline=base.jsonl", "--same=."} {
		var stdout, stderr bytes.Buffer
		if code := run([]string{"--forecast", t.TempDir(), flag}, &stdout, &stderr); code != 2 {
			t.Errorf("%s: exit %d, want 2", flag, code)
		}
		if stdout.Len() != 0 {
			t.Errorf("%s: printed %q", flag, stdout.String())
		}
	}
}
