package main

import (
	_ "embed"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/shgew/togi/tools/trialfacts"
)

// shardWeights holds each session's measured wall seconds with the checks of `--sim-flags "--verify-every 1
// --check-memos"` on, keyed by scenario/split-seed. Only the balance of the shards depends on it: a session it lacks
// weighs the mean of those it has, and stale weights make the shards uneven, never wrong.
//
//go:embed shard-weights.json
var shardWeights []byte

func parseShard(s string) (index, count int, err error) {
	i, n, ok := strings.Cut(s, "/")
	if ok {
		index, err = strconv.Atoi(i)
		if err == nil {
			count, err = strconv.Atoi(n)
		}
	}
	if !ok || err != nil || count < 1 || index < 0 || index >= count {
		return 0, 0, fmt.Errorf("--shard %q must be I/N with 0 <= I < N", s)
	}
	return index, count, nil
}

// shardRuns returns shard index of count: the runs, heaviest first, each dealt to the lightest shard so far, so every
// run is in exactly one shard and the split depends on nothing but the runs and their weights.
func shardRuns(runs []runSpec, weights map[string]float64, index, count int) []runSpec {
	mean := 1.0
	if len(weights) > 0 {
		total := 0.0
		for _, key := range slices.Sorted(maps.Keys(weights)) {
			total += weights[key]
		}
		mean = total / float64(len(weights))
	}
	weight := func(r runSpec) float64 {
		if w, ok := weights[sessionKey{r.scenario.Name, r.split, r.seed}.String()]; ok {
			return w
		}
		return mean
	}
	ordered := slices.Clone(runs)
	slices.SortStableFunc(ordered, func(a, b runSpec) int {
		if wa, wb := weight(a), weight(b); wa != wb {
			if wa > wb {
				return -1
			}
			return 1
		}
		return strings.Compare(sessionKey{a.scenario.Name, a.split, a.seed}.String(), sessionKey{b.scenario.Name, b.split, b.seed}.String())
	})
	load := make([]float64, count)
	var shard []runSpec
	for _, r := range ordered {
		lightest := 0
		for i, l := range load {
			if l < load[lightest] {
				lightest = i
			}
		}
		load[lightest] += weight(r)
		if lightest == index {
			shard = append(shard, r)
		}
	}
	return shard
}

// executeShard runs one shard of the selected sessions with the simulator flags of --sim-flags, and fails unless every
// session of it reaches a normal end: it concludes or ends in a dead end. A failed check of the simulator is an error
// exit. It compares no journal and reports no metrics, so it needs neither a baseline nor a git checkout.
func executeShard(o options, stdout, stderr io.Writer) error {
	index, count, err := parseShard(o.shard)
	if err != nil {
		return err
	}
	var weights map[string]float64
	if err := json.Unmarshal(shardWeights, &weights); err != nil {
		return fmt.Errorf("decode shard weights: %w", err)
	}
	runs, err := loadRuns(o.suite, o.split, trialfacts.Extracts{})
	if err != nil {
		return err
	}
	runs = shardRuns(runs, weights, index, count)
	if len(runs) == 0 {
		return fmt.Errorf("shard %s holds no sessions", o.shard)
	}
	started := time.Now()
	buildDir, err := os.MkdirTemp("", "togi-bench-build-")
	if err != nil {
		return fmt.Errorf("create build directory: %w", err)
	}
	defer os.RemoveAll(buildDir)
	binary, err := buildSimulator(o.ctx, buildDir, stderr)
	if err != nil {
		return err
	}
	runRoot, err := os.MkdirTemp("", "togi-bench-runs-")
	if err != nil {
		return fmt.Errorf("create run directory: %w", err)
	}
	defer os.RemoveAll(runRoot)
	extra := strings.Fields(o.simFlags)
	type outcome struct {
		status string
		wall   float64
		log    string
		err    error
	}
	outcomes := make([]outcome, len(runs))
	queue := make(chan int)
	var wg sync.WaitGroup
	for range min(o.jobs, len(runs)) {
		wg.Go(func() {
			for i := range queue {
				run, err := launchSimulator(o.ctx, binary, runRoot, runs[i], o.maxBoots, o.timeout, extra)
				var log []byte
				if run.dir != "" {
					log, _ = os.ReadFile(filepath.Join(run.dir, "sim.log"))
				}
				if err != nil {
					outcomes[i] = outcome{err: err, log: lastLines(string(log), 5)}
					continue
				}
				status := shardStatus(run, string(log))
				outcomes[i] = outcome{status: status, wall: run.wall}
				if status == "error" || status == "censored" {
					outcomes[i].log = lastLines(string(log), 5)
				}
				_ = os.RemoveAll(run.dir)
			}
		})
	}
	for i := range runs {
		queue <- i
	}
	close(queue)
	wg.Wait()
	var failed []error
	for i, r := range runs {
		key := sessionKey{r.scenario.Name, r.split, r.seed}
		o := outcomes[i]
		switch {
		case o.err != nil:
			fmt.Fprintf(stderr, "%s: %v:\n%s", key, o.err, o.log)
			failed = append(failed, fmt.Errorf("%s: %w", key, o.err))
		case o.status == "error" || o.status == "censored":
			fmt.Fprintf(stderr, "%s %s after %.1fs:\n%s", key, o.status, o.wall, o.log)
			failed = append(failed, fmt.Errorf("%s ended %s", key, o.status))
		default:
			fmt.Fprintf(stdout, "%s %s %.1fs\n", key, o.status, o.wall)
		}
	}
	fmt.Fprintf(stdout, "shard %s: %d sessions, %d failed, harness_wall_s=%.3f\n", o.shard, len(runs), len(failed), time.Since(started).Seconds())
	return errors.Join(failed...)
}

func shardStatus(run simulation, log string) string {
	switch {
	case run.exit == 0:
		return "concluded"
	case run.exit == 1 && containsDeadEnd(log):
		return "deadend"
	case run.exit == 3:
		return "censored"
	}
	return "error"
}

func lastLines(s string, n int) string {
	lines := strings.SplitAfter(strings.TrimRight(s, "\n")+"\n", "\n")
	return strings.Join(lines[max(0, len(lines)-1-n):], "")
}
