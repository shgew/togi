package main

import (
	"bytes"
	"context"
	"errors"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/simrun"
	"github.com/shgew/togi/tools/forecast"
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
		stop, err := simrun.Simulate(context.Background(), simrun.Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Log: io.Discard, Cycles: 1, InMemoryJournal: true})
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
				record.Commit = "0123abc"
				if err := reportForecast(&tables[i], record); err != nil {
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
			path := filepath.Join("testdata", "forecast-"+tc.name+".golden")
			if *update {
				if err := os.WriteFile(path, tables[0].Bytes(), 0o600); err != nil {
					t.Fatal(err)
				}
			}
			want, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(string(want), tables[0].String()); diff != "" {
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
