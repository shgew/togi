// audit checks journal invariants independently of the tuner's avoidance predicate.
package main

import (
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"slices"
	"strings"
	"time"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/tuner"
)

type options struct {
	suite, keep, records, root string
	seeds, jobs                int
	timeout                    time.Duration
}

func main() { os.Exit(run(os.Args[1:], os.Stdout, os.Stderr)) }
func run(args []string, stdout, stderr io.Writer) int {
	var o options
	flags := flag.NewFlagSet("audit", flag.ContinueOnError)
	flags.SetOutput(stderr)
	flags.Usage = func() {
		fmt.Fprintln(stderr, "Usage: audit [--seeds N --jobs N --keep DIR --suite FILE --timeout D]\n       audit [--records FILE --root BENCH-DIR] STATE-DIR...\n\nWithout state directories, keep and audit a simulated sweep (200 seeds per scenario).\nWith state directories, read their current and archived journals without writing.\nViolations are JSON Lines on stdout; summaries and evidence paths are on stderr.")
		flags.PrintDefaults()
	}
	flags.IntVar(&o.seeds, "seeds", 200, "seeds per scenario, numbered from 1")
	flags.IntVar(&o.jobs, "jobs", runtime.NumCPU(), "maximum parallel bench subprocesses")
	flags.StringVar(&o.keep, "keep", "", "retain sweep evidence under DIR (default: a new temporary directory)")
	flags.StringVar(&o.suite, "suite", "tools/bench/suite.toml", "scenario suite; machine paths resolve relative to it")
	flags.DurationVar(&o.timeout, "timeout", 180*time.Second, "wall timeout per simulated run")
	flags.StringVar(&o.records, "records", "", "bench JSON Lines run records for termination and wall-time checks")
	flags.StringVar(&o.root, "root", "", "bench run root containing SCENARIO/SPLIT-SEED; requires --records")
	if err := flags.Parse(args); errors.Is(err, flag.ErrHelp) {
		return 0
	} else if err != nil {
		return 2
	}
	if o.seeds < 1 || o.jobs < 1 || o.timeout <= 0 || (o.records == "") != (o.root == "") {
		fmt.Fprintln(stderr, "audit: require positive seeds, jobs and timeout, and both --records and --root")
		return 2
	}
	dirs := flags.Args()
	if len(dirs) == 0 && o.records == "" {
		var err error
		o.records, o.root, err = sweep(o, stdout, stderr)
		if err != nil {
			fmt.Fprintf(stderr, "audit: %v\n", err)
			return 1
		}
	}
	var records []runRecord
	if o.records != "" {
		var err error
		records, err = readRecords(o.records)
		if err != nil {
			fmt.Fprintf(stderr, "audit: %v\n", err)
			return 1
		}
		if len(dirs) == 0 {
			for _, r := range records {
				dirs = append(dirs, runDirectory(o.root, r))
			}
		}
	}
	// Relative and absolute spellings of one run directory share a record.
	metadata := make(map[string]runRecord)
	for _, r := range records {
		key, err := filepath.Abs(runDirectory(o.root, r))
		if err != nil {
			fmt.Fprintf(stderr, "audit: %v\n", err)
			return 1
		}
		metadata[key] = r
	}
	found := timingViolations(records, o.root)
	journals := 0
	for _, dir := range dirs {
		key, err := filepath.Abs(dir)
		if err != nil {
			fmt.Fprintf(stderr, "audit: %v\n", err)
			return 1
		}
		r, simulated := metadata[key]
		issues, count, err := auditDirectory(dir, simulated)
		journals += count
		if err != nil {
			issues = append(issues, violation{Directory: dir, Check: "journal", Reason: err.Error()})
		}
		for i := range issues {
			issues[i].Scenario, issues[i].Seed = r.Scenario, r.Seed
		}
		found = append(found, issues...)
	}
	encoder := json.NewEncoder(stdout)
	for _, issue := range found {
		if err := encoder.Encode(issue); err != nil {
			fmt.Fprintf(stderr, "audit: write report: %v\n", err)
			return 1
		}
	}
	fmt.Fprintf(stderr, "audit: directories=%d journals=%d violations=%d\n", len(dirs), journals, len(found))
	if len(found) > 0 {
		return 1
	}
	return 0
}

func auditDirectory(dir string, simulated bool) ([]violation, int, error) {
	archives, err := archivePaths(dir)
	if err != nil {
		return nil, 0, err
	}
	paths := archives
	paths = append(paths, filepath.Join(dir, "events.jsonl"))
	var found []violation
	count := 0
	for _, path := range paths {
		events, err := journal.ReadHistory(path)
		if errors.Is(err, os.ErrNotExist) && filepath.Base(path) == "events.jsonl" && len(archives) > 0 {
			continue
		}
		if err != nil {
			found = append(found, violation{Directory: dir, Journal: path, Check: "journal", Reason: err.Error()})
			continue
		}
		count++
		// An archive may end at a transition or an interrupted action; only a
		// finished run's current journal must conclude.
		issues := auditEvents(events, simulated && filepath.Base(path) == "events.jsonl")
		for i := range issues {
			issues[i].Directory, issues[i].Journal = dir, path
		}
		found = append(found, issues...)
		if filepath.Base(path) != "events.jsonl" {
			continue
		}
		cached, err := os.ReadFile(filepath.Join(dir, "state.json"))
		if errors.Is(err, os.ErrNotExist) {
			continue
		}
		seq, session := 0, ""
		if len(events) > 0 {
			seq = events[len(events)-1].Seq
			if p, ok := events[0].Data.(*journal.SessionStart); ok {
				session = p.Session
			}
		}
		issue := violation{Directory: dir, Journal: path, Session: session, Seq: seq, Check: "replay"}
		if err != nil {
			issue.Reason = err.Error()
			found = append(found, issue)
			continue
		}
		var snapshot struct {
			Schema  int `json:"schema"`
			LastSeq int `json:"last_seq"`
		}
		if err := json.Unmarshal(cached, &snapshot); err != nil {
			issue.Reason = fmt.Sprintf("decode state.json: %v", err)
			found = append(found, issue)
			continue
		}
		if snapshot.LastSeq != seq {
			continue
		}
		// Replay needs the current ruleset; an older session has no current projection.
		build := journal.BuildOf(events)
		if build.Ruleset != 0 && build.Ruleset != tuner.Ruleset {
			continue
		}
		// Projection always stamps the current schema, so a snapshot or session from
		// an older schema cannot match it; the next run archives that session.
		if (snapshot.Schema > 0 && snapshot.Schema < journal.Schema) || (build.Schema > 0 && build.Schema < journal.Schema) {
			continue
		}
		// History reading intentionally omits configuration. Replay uses the complete
		// current-ruleset reader instead.
		events, _, err = journal.ReadReplay(dir, tuner.Ruleset)
		if err != nil {
			issue.Reason = fmt.Sprintf("cannot compare state.json projection: %v", err)
			found = append(found, issue)
			continue
		}
		var projected journal.State
		t := tuner.New()
		journal.Replay(events, &projected, t)
		t.Project(&projected)
		fields, err := projectionDifferences(projected, cached)
		if err != nil {
			issue.Reason = err.Error()
			found = append(found, issue)
		} else if len(fields) > 0 {
			issue.Reason = "state.json differs from replay in " + strings.Join(fields, ", ")
			found = append(found, issue)
		}
	}
	return found, count, nil
}

// archivePaths lists archive/*.jsonl literally: a state directory name may contain glob metacharacters.
func archivePaths(dir string) ([]string, error) {
	entries, err := os.ReadDir(filepath.Join(dir, "archive"))
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("list archives: %w", err)
	}
	var paths []string
	for _, entry := range entries {
		if strings.HasSuffix(entry.Name(), ".jsonl") {
			paths = append(paths, filepath.Join(dir, "archive", entry.Name()))
		}
	}
	return paths, nil
}

func projectionDifferences(projected journal.State, cached []byte) ([]string, error) {
	data, err := json.Marshal(projected)
	if err != nil {
		return nil, fmt.Errorf("encode replay projection: %w", err)
	}
	var want, got map[string]any
	if err := json.Unmarshal(data, &want); err != nil {
		return nil, fmt.Errorf("decode replay projection: %w", err)
	}
	if err := json.Unmarshal(cached, &got); err != nil {
		return nil, fmt.Errorf("decode state.json: %w", err)
	}
	var fields []string
	for key, value := range want {
		actual, exists := got[key]
		if !exists || !reflect.DeepEqual(value, actual) {
			fields = append(fields, key)
		}
	}
	for key := range got {
		if _, exists := want[key]; !exists {
			fields = append(fields, key)
		}
	}
	slices.Sort(fields)
	return fields, nil
}
