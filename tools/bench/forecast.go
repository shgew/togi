package main

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"text/tabwriter"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/tuner"
	"github.com/shgew/togi/tools/forecast"
	"github.com/shgew/togi/tools/modelcheck"
	"github.com/shgew/togi/tools/trialfacts"
)

// forecastScenario names the suite scenario whose runs form the forecast ensemble.
const forecastScenario = "target"

func executeForecast(o options, stdout, stderr io.Writer) (err error) {
	anchor, err := forecast.AnchorOf(o.forecast)
	if err != nil {
		return fmt.Errorf("read anchor: %w", err)
	}
	extracts := trialfacts.Extracts{}
	runs, err := ensembleRuns(o.suite, extracts)
	if err != nil {
		return err
	}
	_, checks, err := modelChecks(runs, extracts)
	if err != nil {
		return err
	}
	suiteDir := filepath.Dir(o.suite)
	files, err := ensembleFiles(suiteDir, runs, extracts)
	if err != nil {
		return err
	}
	commit, err := gitOutput("rev-parse", "--short", "HEAD")
	if err != nil {
		return err
	}
	dirty, err := treeDirty(o.out)
	if err != nil {
		return err
	}
	buildDir, err := os.MkdirTemp("", "togi-forecast-build-")
	if err != nil {
		return fmt.Errorf("create build directory: %w", err)
	}
	defer os.RemoveAll(buildDir)
	binary, err := buildSimulator(buildDir, stderr)
	if err != nil {
		return err
	}
	var root string
	if o.keep == "" {
		root, err = os.MkdirTemp("", "togi-forecast-runs-")
	} else if err = os.MkdirAll(o.keep, 0755); err == nil {
		root, err = os.MkdirTemp(o.keep, "forecast-")
	}
	if err != nil {
		return fmt.Errorf("create run directory: %w", err)
	}
	defer func() {
		if err == nil && o.keep == "" {
			os.RemoveAll(root)
			return
		}
		fmt.Fprintf(stderr, "bench: keeping runs in %s\n", root)
	}()
	launch := func(spec runSpec) (simulation, error) { return launchSimulator(binary, root, spec, o.timeout) }
	record, err := makeForecast(o.forecast, root, anchor, runs, suiteDir, o.jobs, o.keep != "", launch)
	if err != nil {
		return err
	}
	record.Commit, record.Dirty, record.Files = commit, dirty, files
	if o.out != "" {
		if err := writeRecord(o.out, record); err != nil {
			return err
		}
	}
	if err := reportForecast(stdout, record); err != nil {
		return err
	}
	modelcheck.Report(stdout, checks)
	return nil
}

// ensembleRuns selects every dev and holdout run of the suite's forecast scenario.
func ensembleRuns(suite string, extracts trialfacts.Extracts) ([]runSpec, error) {
	all, err := loadRuns(suite, "all", extracts)
	if err != nil {
		return nil, err
	}
	var runs []runSpec
	for _, spec := range all {
		if spec.scenario.Name == forecastScenario {
			runs = append(runs, spec)
		}
	}
	if len(runs) == 0 {
		return nil, fmt.Errorf("suite %s has no %s scenario", suite, forecastScenario)
	}
	return runs, nil
}

func writeRecord(path string, record forecast.Record) error {
	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		return fmt.Errorf("create forecast directory: %w", err)
	}
	f, err := os.Create(path)
	if err != nil {
		return fmt.Errorf("create forecast: %w", err)
	}
	if err := record.Write(f); err != nil {
		f.Close()
		return fmt.Errorf("write forecast: %w", err)
	}
	if err := f.Close(); err != nil {
		return fmt.Errorf("close forecast: %w", err)
	}
	return nil
}

// makeForecast runs the ensemble from copies of the state directory input and summarizes what each run did after the
// anchor. The record's commit, dirty flag and files are the caller's.
func makeForecast(input, root string, anchor forecast.Anchor, specs []runSpec, suiteDir string, jobs int, keep bool, launch func(runSpec) (simulation, error)) (forecast.Record, error) {
	runs := make([]forecast.Run, len(specs))
	errs := make([]error, len(specs))
	queue := make(chan int)
	var wg sync.WaitGroup
	for range min(jobs, len(specs)) {
		wg.Go(func() {
			for i := range queue {
				spec := specs[i]
				outcome, err := forecastRun(input, root, anchor, spec, keep, launch)
				if err != nil {
					errs[i] = fmt.Errorf("forecast %s %s-%d: %w", spec.scenario.Name, spec.split, spec.seed, err)
				}
				runs[i] = forecast.Run{Machine: relativeTo(suiteDir, spec.scenario.Machine), Seed: spec.seed, Split: spec.split, Outcome: outcome}
			}
		})
	}
	for i := range specs {
		queue <- i
	}
	close(queue)
	wg.Wait()
	if err := errors.Join(errs...); err != nil {
		return forecast.Record{}, err
	}
	summary, err := forecast.Summarize(runs)
	if err != nil {
		return forecast.Record{}, err
	}
	return forecast.Record{Anchor: anchor, Ruleset: tuner.Ruleset, Runs: runs, Summary: summary}, nil
}

// forecastRun resumes one copy of the state directory on the simulator. A timeout or error fails the forecast.
func forecastRun(input, root string, anchor forecast.Anchor, spec runSpec, keep bool, launch func(runSpec) (simulation, error)) (forecast.Outcome, error) {
	dir := runDir(root, spec)
	if err := copyState(input, dir); err != nil {
		return forecast.Outcome{}, err
	}
	run, err := launch(spec)
	if err != nil {
		return forecast.Outcome{}, err
	}
	sessions, err := forecast.ReadDir(dir, anchor.Session)
	if err != nil {
		return forecast.Outcome{}, err
	}
	outcome, err := forecast.After(sessions, anchor)
	if err != nil {
		return forecast.Outcome{}, err
	}
	log, err := os.ReadFile(filepath.Join(dir, "sim.log"))
	if err != nil && !errors.Is(err, fs.ErrNotExist) {
		return forecast.Outcome{}, fmt.Errorf("read run log: %w", err)
	}
	var live []journal.Event
	if len(sessions) > 0 {
		live = sessions[len(sessions)-1].Events
	}
	switch status := runStatus(run.exit, run.timedOut, live, string(log)); status {
	case forecast.DeadEnd:
		outcome.Status = forecast.DeadEnd
	case forecast.Concluded, forecast.Censored:
		if outcome.Status != status {
			return forecast.Outcome{}, fmt.Errorf("simulator exit %d means %s, but the journal after the anchor is %s", run.exit, status, outcome.Status)
		}
	case "timeout":
		return forecast.Outcome{}, fmt.Errorf("simulator timed out")
	default:
		return forecast.Outcome{}, fmt.Errorf("simulator failed with exit %d", run.exit)
	}
	if !keep {
		if err := os.RemoveAll(dir); err != nil {
			return forecast.Outcome{}, fmt.Errorf("remove run: %w", err)
		}
	}
	return outcome, nil
}

// copyState copies the journals a resumed session reads from src into dst: the live journal, state.json, archived
// journals and reset --all markers. src is only read.
func copyState(src, dst string) error {
	if err := os.MkdirAll(dst, 0755); err != nil {
		return fmt.Errorf("create run %s: %w", dst, err)
	}
	files := []string{"events.jsonl"}
	if _, err := os.Stat(filepath.Join(src, "state.json")); err == nil {
		files = append(files, "state.json")
	} else if !errors.Is(err, fs.ErrNotExist) {
		return fmt.Errorf("read state copy: %w", err)
	}
	entries, err := os.ReadDir(filepath.Join(src, "archive"))
	if err != nil && !errors.Is(err, fs.ErrNotExist) {
		return fmt.Errorf("read archive copy: %w", err)
	}
	for _, entry := range entries {
		name := entry.Name()
		if entry.Type().IsRegular() && (strings.HasSuffix(name, ".jsonl") || strings.HasSuffix(name, "-reset-all")) {
			files = append(files, filepath.Join("archive", name))
		}
	}
	if len(entries) > 0 {
		if err := os.MkdirAll(filepath.Join(dst, "archive"), 0755); err != nil {
			return fmt.Errorf("create run archive: %w", err)
		}
	}
	for _, name := range files {
		data, err := os.ReadFile(filepath.Join(src, name))
		if err != nil {
			return fmt.Errorf("read state copy: %w", err)
		}
		if err := os.WriteFile(filepath.Join(dst, name), data, 0644); err != nil {
			return fmt.Errorf("write run copy: %w", err)
		}
	}
	return nil
}

// ensembleFiles hashes each machine file and facts extract the runs read, in first-use order, by path relative to the
// suite's directory.
func ensembleFiles(suiteDir string, runs []runSpec, extracts trialfacts.Extracts) ([]forecast.File, error) {
	var paths []string
	for _, spec := range runs {
		if spec.scenario.Machine == "" {
			continue
		}
		paths = append(paths, spec.scenario.Machine)
		if spec.cfg.Facts != "" {
			extract, _, err := extracts.Load(spec.scenario.Machine, spec.cfg)
			if err != nil {
				return nil, err
			}
			paths = append(paths, extract)
		}
	}
	var files []forecast.File
	seen := map[string]bool{}
	for _, path := range paths {
		if seen[path] {
			continue
		}
		seen[path] = true
		data, err := os.ReadFile(path)
		if err != nil {
			return nil, fmt.Errorf("hash ensemble file: %w", err)
		}
		sum := sha256.Sum256(data)
		files = append(files, forecast.File{Path: relativeTo(suiteDir, path), SHA256: hex.EncodeToString(sum[:])})
	}
	return files, nil
}

func relativeTo(dir, path string) string {
	if path == "" {
		return ""
	}
	if rel, err := filepath.Rel(dir, path); err == nil {
		return filepath.ToSlash(rel)
	}
	return filepath.ToSlash(path)
}

func reportForecast(w io.Writer, r forecast.Record) error {
	metrics, err := forecast.Metrics(r.Runs)
	if err != nil {
		return err
	}
	commit := r.Commit
	if r.Dirty {
		commit += " (dirty)"
	}
	s := r.Summary
	fmt.Fprintf(w, "forecast: after session %s event %d at %s\n", r.Anchor.Session, r.Anchor.Seq, r.Anchor.Time.UTC().Format(time.RFC3339))
	fmt.Fprintf(w, "forecast: commit %s, ruleset %d, %d %s runs: %d concluded, %d dead ends, %d censored\n", commit, r.Ruleset, s.Runs, forecastScenario, s.Concluded, s.DeadEnds, s.Censored)
	fmt.Fprintf(w, "forecast: hours run from the anchor to conclusion, over concluded runs only; %s\n", forecast.RecoveryBias())
	tab := tabwriter.NewWriter(w, 0, 4, 2, ' ', 0)
	fmt.Fprintln(tab, "metric\tmedian\tp10..p90\tmin..max")
	for _, m := range metrics {
		if m.Range == nil {
			fmt.Fprintf(tab, "%s\t-\t-\t-\n", m.Name)
			continue
		}
		fmt.Fprintf(tab, "%s\t%s\t%s..%s\t%s..%s\n", m.Name, m.Format(m.Range.Median), m.Format(m.Range.P10), m.Format(m.Range.P90), m.Format(m.Range.Min), m.Format(m.Range.Max))
	}
	return tab.Flush()
}
