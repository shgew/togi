package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/simrun"
	"github.com/shgew/togi/internal/tuner"
	"github.com/shgew/togi/tools/forecast"
)

// simulate runs a simulated session in dir, resuming its journal, until it concludes, reaches maxBoots or until
// accepts an event.
func simulate(t *testing.T, dir string, seed uint64, maxBoots int, until func(journal.Event) bool) {
	t.Helper()
	cfg, err := sim.Resume(dir, sim.Config{Seed: seed})
	if err != nil {
		t.Fatal(err)
	}
	m, err := sim.New(cfg)
	if err != nil {
		t.Fatal(err)
	}
	_, err = simrun.Simulate(context.Background(), simrun.Input{Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m, Log: io.Discard, Cycles: 1, InMemoryJournal: true, MaxBoots: maxBoots, Until: until})
	if err != nil && !errors.Is(err, simrun.ErrBootCap) {
		t.Fatal(err)
	}
}

// resumedCopy returns a forecast whose runs have statuses for a session stopped at its first crash, and a copy of that
// session resumed with another seed for at most maxBoots boots.
func resumedCopy(t *testing.T, maxBoots int, statuses ...string) (string, string) {
	t.Helper()
	copied := t.TempDir()
	simulate(t, copied, 42, 0, func(e journal.Event) bool { return e.Kind == journal.KindCrashDetected })
	anchor, err := forecast.AnchorOf(copied)
	if err != nil {
		t.Fatal(err)
	}
	after := t.TempDir()
	for _, name := range []string{"events.jsonl", "state.json"} {
		data, err := os.ReadFile(filepath.Join(copied, name))
		if err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(after, name), data, 0o644); err != nil {
			t.Fatal(err)
		}
	}
	simulate(t, after, 7, maxBoots, nil)
	profiles := [][]int{
		{-20, -30, -14, -23, -25, -7, -17, -5, -21, -16, -13, -4, -12, -24, -7, -18},
		{-34, -34, -6, -17, -12, -9, -27, -6, -16, -34, -7, -12, -18, -34, -22, -34},
		{-17, -6, -14, -23, -25, -2, -10, -4, -21, -2, -13, -1, -7, -22, 0, -1},
	}
	record := forecast.Record{Anchor: anchor, Commit: "dev", Ruleset: tuner.Ruleset}
	for i, status := range statuses {
		o := forecast.Outcome{Status: status, Hours: 20 + float64(i), Crashes: 10 + 3*i, Hunts: i, Profile: profiles[i]}
		for _, offset := range o.Profile {
			o.Depth += offset
		}
		record.Runs = append(record.Runs, forecast.Run{Machine: "machines/target.toml", Seed: uint64(i + 1), Split: "dev", Outcome: o})
	}
	record.Summary, err = forecast.Summarize(record.Runs)
	if err != nil {
		t.Fatal(err)
	}
	var b bytes.Buffer
	if err := record.Write(&b); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(t.TempDir(), "forecast.json")
	if err := os.WriteFile(path, b.Bytes(), 0o644); err != nil {
		t.Fatal(err)
	}
	return path, after
}

func TestScoreResumedSimulatedJournal(t *testing.T) {
	for _, tc := range []struct {
		name     string
		maxBoots int
		statuses []string
		censored bool
	}{
		{"concluded", 0, []string{forecast.Concluded, forecast.Concluded, forecast.DeadEnd}, false},
		{"censored", 14, []string{forecast.Concluded, forecast.Concluded, forecast.DeadEnd}, true},
		{"unconcluded", 14, []string{forecast.DeadEnd, forecast.Censored, forecast.DeadEnd}, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			record, after := resumedCopy(t, tc.maxBoots, tc.statuses...)
			var out, errOut bytes.Buffer
			if err := run([]string{"--state-dir", after, "--forecast", record}, &out, &errOut); err != nil {
				t.Fatal(err)
			}
			for _, want := range []string{", unchanged\n", "\nForecast score\n"} {
				if !strings.Contains(out.String(), want) {
					t.Errorf("score lacks %q:\n%s", want, &out)
				}
			}
			if got := strings.Contains(out.String(), "\nRun: censored "); got != tc.censored {
				t.Errorf("run censored = %t, want %t:\n%s", got, tc.censored, &out)
			}
		})
	}
}

// TestRenderScoreRecordedScores renders scores captured once from simulated runs, whose build stamps are literal, so
// a tuner change leaves the goldens alone.
func TestRenderScoreRecordedScores(t *testing.T) {
	for _, name := range []string{"concluded", "censored", "unconcluded"} {
		t.Run(name, func(t *testing.T) {
			data, err := os.ReadFile(filepath.Join("testdata", "forecast-"+name+".json"))
			if err != nil {
				t.Fatal(err)
			}
			var score forecast.Score
			if err := json.Unmarshal(data, &score); err != nil {
				t.Fatal(err)
			}
			metrics, err := forecast.Metrics(score.Record.Runs)
			if err != nil {
				t.Fatal(err)
			}
			if len(metrics) != len(score.Rows) {
				t.Fatalf("score has %d rows, its forecast %d metrics", len(score.Rows), len(metrics))
			}
			for i := range score.Rows {
				score.Rows[i].Metric = metrics[i]
			}
			var out bytes.Buffer
			if err := renderScore(&out, score); err != nil {
				t.Fatal(err)
			}
			checkGolden(t, "forecast-"+name+".golden", out.Bytes())
		})
	}
}

func TestScoreRefusesChangedAnchor(t *testing.T) {
	record, after := resumedCopy(t, 1, forecast.Concluded)
	path := filepath.Join(after, "events.jsonl")
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	data = bytes.Replace(data, []byte(`"boot":"`), []byte(`"boot":"x`), 1)
	if err := os.WriteFile(path, data, 0o644); err != nil {
		t.Fatal(err)
	}
	var out, errOut bytes.Buffer
	err = run([]string{"--state-dir", after, "--forecast", record}, &out, &errOut)
	if !errors.Is(err, forecast.ErrChangedAnchor) {
		t.Fatalf("error %v, want a changed anchor", err)
	}
	if out.Len() != 0 {
		t.Fatalf("scored a changed anchor: %s", &out)
	}
}

func TestForecastRejectsJournalAndSince(t *testing.T) {
	for _, args := range [][]string{{"--journal", "events.jsonl"}, {"--since", "2026-10-07T00:00:00Z"}} {
		err := run(append(args, "--forecast", "forecast.json"), io.Discard, io.Discard)
		if err == nil || !strings.Contains(err.Error(), "cannot be combined") {
			t.Errorf("%v: error %v", args, err)
		}
	}
}
