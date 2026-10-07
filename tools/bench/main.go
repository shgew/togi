package main

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/BurntSushi/toml"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/tuner"
	"github.com/shgew/togi/tools/modelcheck"
	"github.com/shgew/togi/tools/trialfacts"
)

type scenario struct {
	Name     string   `toml:"name"`
	Machine  string   `toml:"machine"`
	Machines []string `toml:"machines"`
	Replay   bool     `toml:"replay"`
	Dev      []uint64 `toml:"dev"`
	Holdout  []uint64 `toml:"holdout"`
}
type runSpec struct {
	scenario scenario
	seed     uint64
	split    string
	cfg      sim.Config
}
type options struct {
	suite, split, out, baseline, keep, same, forecast string

	jobs    int
	timeout time.Duration
}

func main() { os.Exit(run(os.Args[1:], os.Stdout, os.Stderr)) }

func run(args []string, stdout, stderr io.Writer) int {
	var o options
	flags := flag.NewFlagSet("bench", flag.ContinueOnError)
	flags.SetOutput(stderr)
	flags.StringVar(&o.suite, "suite", "tools/bench/suite.toml", "scenario TOML file; relative paths resolve in each tree with --same; machine paths are relative to this file")
	flags.StringVar(&o.split, "split", "dev", "seed split: dev, holdout, or all; incompatible with --same")
	flags.StringVar(&o.out, "out", "", "write one JSON object per run to this file, or with --forecast the forecast record; incompatible with --same")
	flags.StringVar(&o.baseline, "baseline", "", "compare against a JSON Lines baseline; incompatible with --same")
	flags.StringVar(&o.keep, "keep", "", "keep run directories under this directory; --same separates base and head")
	flags.StringVar(&o.same, "same", "", "compare all session journals against checkout DIR, ignoring only build version, revision and description; skip metrics and model checks")
	flags.StringVar(&o.forecast, "forecast", "", "forecast a real run from a copy of its state directory DIR with the suite's target scenario, counting only events after its last; the copy is read, never written; incompatible with --split, --baseline and --same")
	flags.IntVar(&o.jobs, "jobs", runtime.NumCPU(), "maximum parallel simulator subprocesses")
	flags.DurationVar(&o.timeout, "timeout", 180*time.Second, "wall timeout for each simulator subprocess")
	if err := flags.Parse(args); errors.Is(err, flag.ErrHelp) {
		return 0
	} else if err != nil {
		return 2
	}
	if o.same != "" || o.forecast != "" {
		mode, conflicts := "same", []string{"split", "baseline", "out"}
		if o.forecast != "" {
			mode, conflicts = "forecast", []string{"split", "baseline", "same"}
		}
		conflict := ""
		flags.Visit(func(f *flag.Flag) {
			if slices.Contains(conflicts, f.Name) {
				conflict = f.Name
			}
		})
		if conflict != "" {
			fmt.Fprintf(stderr, "bench: --%s cannot be combined with --%s\n", mode, conflict)
			return 2
		}
	}
	if flags.NArg() != 0 || o.jobs < 1 || o.timeout <= 0 || (o.split != "dev" && o.split != "holdout" && o.split != "all") {
		fmt.Fprintln(stderr, "bench: require no positional arguments, positive --jobs and --timeout, and --split dev|holdout|all")
		return 2
	}
	if o.same != "" {
		different, err := executeSame(o, stdout, stderr)
		if err != nil {
			fmt.Fprintf(stderr, "bench: %v\n", err)
			return 1
		}
		if different {
			return 1
		}
		return 0
	}
	if o.forecast != "" {
		if err := executeForecast(o, stdout, stderr); err != nil {
			fmt.Fprintf(stderr, "bench: %v\n", err)
			return 1
		}
		return 0
	}
	if err := execute(o, stdout, stderr); err != nil {
		fmt.Fprintf(stderr, "bench: %v\n", err)
		return 1
	}
	return 0
}

func loadRuns(path, split string, extracts trialfacts.Extracts) ([]runSpec, error) {
	var suite struct {
		Scenarios []scenario `toml:"scenario"`
	}
	md, err := toml.DecodeFile(path, &suite)
	if err != nil {
		return nil, fmt.Errorf("load suite: %w", err)
	}
	if len(md.Undecoded()) > 0 {
		return nil, fmt.Errorf("unknown suite key %s", md.Undecoded()[0])
	}
	seen := make(map[string]bool)
	var runs []runSpec
	for _, s := range suite.Scenarios {
		if s.Name == "" || seen[s.Name] || filepath.Base(s.Name) != s.Name || s.Name == "." || s.Name == ".." {
			return nil, fmt.Errorf("invalid or duplicate scenario %q", s.Name)
		}
		seen[s.Name] = true
		paths := s.Machines
		if s.Machine != "" && len(paths) > 0 {
			return nil, fmt.Errorf("load scenario %s: machine and machines are mutually exclusive", s.Name)
		}
		if len(paths) == 0 {
			paths = []string{s.Machine}
		}
		configs := make([]sim.Config, len(paths))
		resolved := make([]string, len(paths))
		for i, machinePath := range paths {
			if machinePath != "" {
				if !filepath.IsAbs(machinePath) {
					machinePath = filepath.Join(filepath.Dir(path), machinePath)
				}
				configs[i], err = sim.LoadMachine(machinePath)
				if err != nil {
					return nil, fmt.Errorf("load scenario %s: %w", s.Name, err)
				}
			}
			resolved[i] = machinePath
			if s.Replay {
				configs[i].Replay, err = extracts.Replay(machinePath, configs[i])
				if err != nil {
					return nil, fmt.Errorf("load scenario %s: %w", s.Name, err)
				}
			}
		}
		seeds := make(map[uint64]bool)
		for _, group := range []struct {
			name  string
			seeds []uint64
		}{{"dev", s.Dev}, {"holdout", s.Holdout}} {
			for index, seed := range group.seeds {
				if seeds[seed] {
					return nil, fmt.Errorf("scenario %s repeats seed %d", s.Name, seed)
				}
				seeds[seed] = true
				if split == "all" || split == group.name {
					member := index % len(configs)
					selected := s
					selected.Machine = resolved[member]
					runs = append(runs, runSpec{selected, seed, group.name, configs[member]})
				}
			}
		}
	}
	if len(runs) == 0 {
		return nil, errors.New("suite has no selected runs")
	}
	return runs, nil
}

func gitOutput(args ...string) (string, error) {
	b, err := exec.Command("git", args...).Output()
	if err != nil {
		return "", fmt.Errorf("git %s: %w", strings.Join(args, " "), err)
	}
	return strings.TrimSpace(string(b)), nil
}

func treeDirty(out string) (bool, error) {
	root, err := gitOutput("rev-parse", "--show-toplevel")
	if err != nil {
		return false, err
	}
	abs := ""
	if out != "" {
		abs, err = filepath.Abs(out)
		if err != nil {
			return false, fmt.Errorf("resolve output: %w", err)
		}
	}
	b, err := exec.Command("git", "status", "--porcelain", "-z", "--untracked-files=all").Output()
	if err != nil {
		return false, fmt.Errorf("git status: %w", err)
	}
	entries := strings.Split(string(b), "\x00")
	for i := 0; i < len(entries); i++ {
		e := entries[i]
		if len(e) < 4 {
			continue
		}
		if filepath.Join(root, e[3:]) != abs {
			return true, nil
		}
		if strings.ContainsAny(e[:2], "RC") {
			i++
			if i < len(entries) && filepath.Join(root, entries[i]) != abs {
				return true, nil
			}
		}
	}
	return false, nil
}

func execute(o options, stdout, stderr io.Writer) error {
	started := time.Now()
	extracts := trialfacts.Extracts{}
	runs, err := loadRuns(o.suite, o.split, extracts)
	if err != nil {
		return err
	}
	checksByMachine, checks, err := modelChecks(runs, extracts)
	if err != nil {
		return err
	}
	var baseline []result
	if o.baseline != "" {
		baseline, err = readResults(o.baseline)
		if err != nil {
			return err
		}
	}
	commit, err := gitOutput("rev-parse", "--short", "HEAD")
	if err != nil {
		return err
	}
	dirty, err := treeDirty(o.out)
	if err != nil {
		return err
	}
	buildDir, err := os.MkdirTemp("", "togi-bench-build-")
	if err != nil {
		return fmt.Errorf("create build directory: %w", err)
	}
	defer os.RemoveAll(buildDir)
	binary, err := buildSimulator(buildDir, stderr)
	if err != nil {
		return err
	}
	var runRoot string
	if o.keep == "" {
		runRoot, err = os.MkdirTemp("", "togi-bench-runs-")
	} else {
		err = os.MkdirAll(o.keep, 0755)
		if err == nil {
			runRoot, err = os.MkdirTemp(o.keep, "bench-")
		}
	}
	if err != nil {
		return fmt.Errorf("create run directory: %w", err)
	}
	if o.keep == "" {
		defer os.RemoveAll(runRoot)
	} else {
		fmt.Fprintf(stderr, "bench: keeping runs in %s\n", runRoot)
	}
	results := make([]result, len(runs))
	errs := make([]error, len(runs))
	queue := make(chan int)
	var wg sync.WaitGroup
	for range min(o.jobs, len(runs)) {
		wg.Go(func() {
			for i := range queue {
				r, err := simulate(binary, runRoot, runs[i], o.timeout, o.keep != "")
				r.Commit, r.Dirty, r.Ruleset = commit, dirty, tuner.Ruleset
				r.ModelCheck = checksByMachine[runs[i].scenario.Machine]
				results[i], errs[i] = r, err
			}
		})
	}
	for i := range runs {
		queue <- i
	}
	close(queue)
	wg.Wait()
	for _, err := range errs {
		if err != nil {
			return err
		}
	}
	setScenarioShares(results)
	if o.out != "" {
		f, err := os.Create(o.out)
		if err != nil {
			return fmt.Errorf("create output: %w", err)
		}
		encoder := json.NewEncoder(f)
		for _, r := range results {
			if err := encoder.Encode(r); err != nil {
				f.Close()
				return fmt.Errorf("write output: %w", err)
			}
		}
		if err := f.Close(); err != nil {
			return fmt.Errorf("close output: %w", err)
		}
	}
	reportSummary(stdout, results)
	modelcheck.Report(stdout, checks)
	if o.baseline != "" {
		reportComparison(stdout, results, baseline)
	}
	fmt.Fprintf(stdout, "harness_wall_s=%.3f\n", time.Since(started).Seconds())
	return nil
}

// modelChecks checks each distinct machine with a facts extract against it, once.
func modelChecks(runs []runSpec, extracts trialfacts.Extracts) (map[string]*modelcheck.Result, []*modelcheck.Result, error) {
	byMachine := make(map[string]*modelcheck.Result)
	var checks []*modelcheck.Result
	for _, spec := range runs {
		if spec.cfg.Facts == "" || byMachine[spec.scenario.Machine] != nil {
			continue
		}
		check, err := modelcheck.Check(spec.scenario.Machine, spec.cfg, extracts)
		if err != nil {
			return nil, nil, fmt.Errorf("check model %s: %w", spec.scenario.Machine, err)
		}
		byMachine[spec.scenario.Machine] = check
		checks = append(checks, check)
	}
	return byMachine, checks, nil
}

// buildSimulator builds tools/sim from the current tree into dir.
func buildSimulator(dir string, stderr io.Writer) (string, error) {
	binary := filepath.Join(dir, "sim")
	build := exec.Command("go", "build", "-o", binary, "./tools/sim")
	build.Stdout, build.Stderr = stderr, stderr
	if err := build.Run(); err != nil {
		return "", fmt.Errorf("build simulator: %w", err)
	}
	return binary, nil
}

type simulation struct {
	dir      string
	exit     int
	wall     float64
	timedOut bool
}

func launchSimulator(binary, root string, spec runSpec, timeout time.Duration) (simulation, error) {
	dir := runDir(root, spec)
	if err := os.MkdirAll(dir, 0755); err != nil {
		return simulation{}, fmt.Errorf("create run %s: %w", dir, err)
	}
	log, err := os.Create(filepath.Join(dir, "sim.log"))
	if err != nil {
		return simulation{}, fmt.Errorf("create run log: %w", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	args := []string{"--seed", fmt.Sprint(spec.seed), "--state-dir", dir}
	if spec.scenario.Machine != "" {
		args = append(args, "--machine", spec.scenario.Machine)
	}
	if spec.scenario.Replay {
		args = append(args, "--replay-facts")
	}
	cmd := exec.CommandContext(ctx, binary, args...)
	cmd.Stdout, cmd.Stderr = log, log
	started := time.Now()
	err = cmd.Run()
	wall := time.Since(started).Seconds()
	timedOut := errors.Is(ctx.Err(), context.DeadlineExceeded)
	if closeErr := log.Close(); closeErr != nil {
		return simulation{}, fmt.Errorf("close run log: %w", closeErr)
	}
	exit := 0
	if err != nil {
		var exited *exec.ExitError
		switch {
		case errors.As(err, &exited):
			exit = exited.ExitCode()
		case timedOut:
			exit = -1
		default:
			return simulation{}, fmt.Errorf("start simulator: %w", err)
		}
	}
	return simulation{dir: dir, exit: exit, wall: wall, timedOut: timedOut}, nil
}

func runDir(root string, spec runSpec) string {
	return filepath.Join(root, spec.scenario.Name, fmt.Sprintf("%s-%d", spec.split, spec.seed))
}

func simulate(binary, root string, spec runSpec, timeout time.Duration, keep bool) (result, error) {
	run, err := launchSimulator(binary, root, spec, timeout)
	if err != nil {
		return result{}, err
	}
	if !keep {
		defer os.RemoveAll(run.dir)
	}
	events, _, err := journal.Read(run.dir)
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return result{}, fmt.Errorf("read run journal: %w", err)
	}
	cfg := spec.cfg
	cfg.Seed = spec.seed
	m, err := sim.New(cfg)
	if err != nil {
		return result{}, fmt.Errorf("create metric machine: %w", err)
	}
	cores := cfg.Cores
	if cores == 0 {
		cores = 16
	}
	r := metrics(events, m, cores)
	text, err := os.ReadFile(filepath.Join(run.dir, "sim.log"))
	if err != nil {
		return result{}, fmt.Errorf("read run log: %w", err)
	}
	r.Scenario, r.Seed, r.Split = spec.scenario.Name, spec.seed, spec.split
	r.Machine = spec.scenario.Machine
	r.ExitCode, r.WallS = run.exit, run.wall
	r.Status = runStatus(run.exit, run.timedOut, events, string(text))
	return r, nil
}

func containsDeadEnd(log string) bool { return strings.Contains(log, "sim: dead end ") }

func readResults(path string) ([]result, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, fmt.Errorf("open baseline: %w", err)
	}
	defer f.Close()
	decoder := json.NewDecoder(f)
	seen := make(map[key]bool)
	var results []result
	for {
		var r result
		if err := decoder.Decode(&r); errors.Is(err, io.EOF) {
			break
		} else if err != nil {
			return nil, fmt.Errorf("decode baseline: %w", err)
		}
		k := key{r.Scenario, r.Seed}
		if seen[k] {
			return nil, fmt.Errorf("duplicate baseline run %s/%d", r.Scenario, r.Seed)
		}
		seen[k] = true
		results = append(results, r)
	}
	return results, nil
}
