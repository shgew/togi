package main

import (
	"bytes"
	"errors"
	"flag"
	"fmt"
	"io"
	"maps"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"time"

	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/tools/modelcheck"
	"github.com/shgew/togi/tools/trialfacts"
)

func main() {
	os.Exit(run(os.Args[1:], os.Stdout, os.Stderr))
}

func run(args []string, stdout, stderr io.Writer) int {
	return runWithSharedVoltageFit(args, stdout, stderr, fitSharedVoltage)
}

func runWithSharedVoltageFit(args []string, stdout, stderr io.Writer, fitter sharedVoltageFit) int {
	flags := flag.NewFlagSet("fit", flag.ContinueOnError)
	flags.SetOutput(stderr)
	extract := flags.String("facts", "tools/bench/facts/target.jsonl.gz", "compressed decisive-facts extract")
	out := flags.String("out", "tools/bench/machines", "output directory for fitted machine files")
	seed := flags.Uint64("seed", 263, "fixed bootstrap seed")
	refits := flags.Int("bootstrap", 8, "number of whole-trial bootstrap refits")
	jobs := flags.Int("jobs", runtime.NumCPU(), "maximum parallel fits (candidate scoring inside a fit uses every CPU)")
	forwardOnly := flags.Bool("forward-only", false, "run only the forward-chained check: fit no ensemble and write no machine files")
	seal := flags.Int("seal", 0, "with --forward-only, leave the newest N sessions out of the forward-chained check")
	sharedVoltage := flags.Bool("shared-voltage-in-sample", false, "fit all decisive facts to target-shared-voltage.toml; in-sample only, not forward-validated; incompatible with --forward-only, --seal, --seed and --bootstrap")
	if err := flags.Parse(args); errors.Is(err, flag.ErrHelp) {
		return 0
	} else if err != nil {
		return 2
	}
	if *sharedVoltage {
		conflict := ""
		flags.Visit(func(f *flag.Flag) {
			switch f.Name {
			case "forward-only", "seal", "seed", "bootstrap":
				conflict = f.Name
			}
		})
		if conflict != "" {
			fmt.Fprintf(stderr, "fit: --shared-voltage-in-sample cannot be combined with --%s\n", conflict)
			return 2
		}
	}
	if flags.NArg() != 0 || *refits < 0 || *jobs < 1 || *seal < 0 || (*seal > 0 && !*forwardOnly) {
		fmt.Fprintln(stderr, "fit: require no positional arguments, a nonnegative --bootstrap, positive --jobs, and a nonnegative --seal only with --forward-only")
		return 2
	}
	var err error
	switch {
	case *sharedVoltage:
		err = generateSharedVoltageAnchor(*extract, *out, stdout, fitter)
	case *forwardOnly:
		err = forward(*extract, *seal, *jobs, stdout)
	default:
		_, err = generate(*extract, *out, *seed, *refits, *jobs, stdout)
	}
	if err != nil {
		fmt.Fprintf(stderr, "fit: %v\n", err)
		return 1
	}
	return 0
}

type fitted struct {
	sample      []trialfacts.Record
	cfg         sim.Config
	loss        float64
	constrained []modelcheck.Group
	flagged     []modelcheck.Group
	check       *modelcheck.Result
}

func generate(extract, out string, seed uint64, refits, jobs int, stdout io.Writer) ([]fitted, error) {
	started := time.Now()
	records, err := trialfacts.Read(extract)
	if err != nil {
		return nil, err
	}
	trials, err := decisive(records)
	if err != nil {
		return nil, err
	}
	absExtract, err := filepath.Abs(extract)
	if err != nil {
		return nil, fmt.Errorf("resolve extract: %w", err)
	}
	absOut, err := filepath.Abs(out)
	if err != nil {
		return nil, fmt.Errorf("resolve output directory: %w", err)
	}
	rel, err := filepath.Rel(absOut, absExtract)
	if err != nil {
		return nil, fmt.Errorf("relativize extract: %w", err)
	}
	if err := os.MkdirAll(out, 0o755); err != nil {
		return nil, fmt.Errorf("create output directory: %w", err)
	}
	forwarded := startForwardCheck(trials, 0, jobs)
	forwardPending := true
	defer func() {
		if forwardPending {
			<-forwarded
		}
	}()
	fits, err := fitParallel(refits+1, jobs, func(n int) (fitted, error) {
		sample := trials
		if n > 0 {
			sample = bootstrap(trials, seed+uint64(n))
		}
		cfg, loss := fit(sample)
		return fitted{sample: sample, cfg: cfg, loss: loss}, nil
	})
	if err != nil {
		return nil, err
	}
	checker, err := modelcheck.NewChecker(fits[0].cfg, records)
	if err != nil {
		return nil, fmt.Errorf("fit 0 model checker: %w", err)
	}
	base := cloneMachine(fits[0].cfg)
	baseMachine, err := sim.New(base)
	if err != nil {
		return nil, fmt.Errorf("fit 0 simulator: %w", err)
	}
	baseAccepted := checker.Accepts(baseMachine)
	fits, err = fitParallel(len(fits), jobs, func(n int) (fitted, error) {
		result := fits[n]
		cfg, loss := result.cfg, result.loss
		path := filepath.Join(out, fmt.Sprintf("target-fit-%d.toml", n))
		m, err := sim.New(cfg)
		if err != nil {
			return fitted{}, fmt.Errorf("fit %d simulator: %w", n, err)
		}
		check := checker.Check(path, extract, m)
		var constrained []modelcheck.Group
		if n > 0 && baseAccepted && check.Status == "flagged" {
			constrained = flaggedGroups(check)
			cfg, loss = fitFrom(result.sample, &base, checker)
			if m, err = sim.New(cfg); err != nil {
				return fitted{}, fmt.Errorf("fit %d constrained simulator: %w", n, err)
			}
			check = checker.Check(path, extract, m)
		}
		result.cfg, result.loss, result.constrained, result.flagged = cfg, loss, constrained, flaggedGroups(check)
		return result, nil
	})
	if err != nil {
		return nil, err
	}
	checks := make([]*modelcheck.Result, len(fits))
	for n := range fits {
		result := &fits[n]
		cfg, loss, constrained := result.cfg, result.loss, result.constrained
		path := filepath.Join(out, fmt.Sprintf("target-fit-%d.toml", n))
		for _, group := range constrained {
			fmt.Fprintf(stdout, "Refit %d constraint: %s %s cores=%v duration=%ds depth=%d n=%d k=%d unconstrained_interval=%v mean_p=%.6g\n", n, group.Class.Regime, group.Class.Workload, group.Class.Cores, group.Class.DurationS, group.Depth, group.N, group.K, group.Interval, group.MeanP)
		}
		if n > 0 && !baseAccepted && len(result.flagged) > 0 {
			fmt.Fprintf(stdout, "Refit %d written unconstrained: the all-facts fit fails the model check, so there is no passing fit to restart from\n", n)
		}
		cfg.Facts = filepath.ToSlash(rel)
		content := encodeMachine(cfg, n, seed, len(result.sample), loss, constrained, result.flagged)
		if err := os.WriteFile(path, content, 0o644); err != nil {
			return nil, fmt.Errorf("fit %d write fitted machine: %w", n, err)
		}
		loaded, err := sim.LoadMachine(path)
		if err != nil {
			return nil, fmt.Errorf("fit %d load fitted machine: %w", n, err)
		}
		m, err := sim.New(loaded)
		if err != nil {
			return nil, fmt.Errorf("fit %d loaded simulator: %w", n, err)
		}
		result.check = checker.Check(path, extract, m)
		checks[n] = result.check
		fmt.Fprintf(stdout, "%s: %d trials; negative log likelihood %.6f\n", path, len(result.sample), loss)
		for j, joint := range cfg.Joints {
			ccd := 0
			for core := range cfg.Cores {
				if _, ok := joint.Members[core]; ok {
					ccd = core / (cfg.Cores / 2)
					break
				}
			}
			fmt.Fprintf(stdout, "  CCD%d joint%d: members=%v rate=%.9g/s after=%.9gs\n", ccd, j, joint.Members, joint.Rate, joint.AfterS)
		}
		reportSignals(stdout, cfg.Model)
	}
	modelcheck.Report(stdout, checks)
	outcome := <-forwarded
	forwardPending = false
	if err := writeForwardCheck(stdout, outcome, 0); err != nil {
		return nil, err
	}
	fmt.Fprintf(stdout, "Fit elapsed: %s\n", time.Since(started).Round(time.Millisecond))
	return fits, nil
}

func flaggedGroups(check *modelcheck.Result) []modelcheck.Group {
	var flagged []modelcheck.Group
	for _, group := range check.Groups {
		if group.Flagged {
			flagged = append(flagged, group)
		}
	}
	return flagged
}

func encodeMachine(cfg sim.Config, index int, seed uint64, trials int, loss float64, constrained, flagged []modelcheck.Group) []byte {
	var b bytes.Buffer
	fmt.Fprintln(&b, "# Generated by just fit; do not hand-edit.")
	fmt.Fprintf(&b, "# Whole-trial bootstrap: index=%d (0=all facts), seed=%d, trials=%d, negative_log_likelihood=%.9g\n", index, seed, trials, loss)
	fmt.Fprintln(&b, "# Fitted: alone/together regime limits, shared rate/growth/near hazard, per-core flat hazard,")
	fmt.Fprintln(&b, "# supported workload overrides, idle limits, and layered R7 CCD joints.")
	if cfg.CCD != nil {
		fmt.Fprintln(&b, "# Fitted CCD residual: joint-gated loaded hazard with shared depth slope and shrunk CCD effects.")
	}
	encodeSignalsComment(&b, cfg.Model)
	fmt.Fprintln(&b, "# Unsupported limits stay -50; unobserved idle exposure cannot identify idle hazards.")
	fmt.Fprintln(&b, "# Fixed: onset boost=0, joint delay=0; default MCE/reset.")
	for _, group := range constrained {
		fmt.Fprintf(&b, "# Constrained bootstrap: %s %s cores=%v duration=%ds depth=%d n=%d k=%d\n", group.Class.Regime, group.Class.Workload, group.Class.Cores, group.Class.DurationS, group.Depth, group.N, group.K)
	}
	if len(flagged) == 0 {
		fmt.Fprintln(&b, "# Model check against the original extract: ok, no flagged groups.")
	} else {
		fmt.Fprintln(&b, "# Model check against the original extract: flagged; this member cannot support a target-machine claim.")
	}
	for _, group := range flagged {
		fmt.Fprintf(&b, "# Flagged: %s %s cores=%v duration=%ds depth=%d n=%d k=%d interval=[%d,%d]\n", group.Class.Regime, group.Class.Workload, group.Class.Cores, group.Class.DurationS, group.Depth, group.N, group.K, group.Interval[0], group.Interval[1])
	}
	encodeConfiguration(&b, cfg)
	return b.Bytes()
}

func encodeConfiguration(b *bytes.Buffer, cfg sim.Config) {
	fmt.Fprintf(b, "cores = %d\nfacts = %s\n", cfg.Cores, strconv.Quote(cfg.Facts))
	if cfg.BIOSContext != (machine.BIOSContext{}) {
		c := cfg.BIOSContext
		fmt.Fprintf(b, "\n[bios_context]\nbios_version = %s\nboard = %s\ncpu_model = %s\nmicrocode = %s\nboost_limit_mhz = %d\n", strconv.Quote(c.BIOSVersion), strconv.Quote(c.Board), strconv.Quote(c.CPUModel), strconv.Quote(c.Microcode), c.BoostLimitMHz)
	}
	m := cfg.Model
	fmt.Fprintf(b, "\n[model]\npast_limit_rate = %.17g\ngrowth = %.17g\nnear_limit_rate = %.17g\nonset_boost = 0.0\n", m.PastLimitRate, m.Growth, m.NearLimitRate)
	if m.RegimeSignals != nil {
		fmt.Fprintf(b, "signals = %s\n\n[model.regime_signals]\n", encodeSignals(m.Signals))
		for _, regime := range slices.Sorted(maps.Keys(m.RegimeSignals)) {
			fmt.Fprintf(b, "%s = %s\n", strconv.Quote(string(regime)), encodeSignals(m.RegimeSignals[regime]))
		}
	}
	if c := cfg.CCD; c != nil {
		fmt.Fprintf(b, "\n[ccd]\nlog_rate = %.17g\nslope = %.17g\neffect = [%.17g, %.17g]\n", c.LogRate, c.Slope, c.Effect[0], c.Effect[1])
	}
	for core, limit := range cfg.Limits {
		fmt.Fprintf(b, "\n[[core]]\nid = %d\nalone = [%d, %d, %d, %d, %d]\ntogether = [%d, %d, %d, %d, %d, %d, %d]\nflat = %.17g\n", core, limit.Alone[0], limit.Alone[1], limit.Alone[2], limit.Alone[3], limit.Alone[4], limit.Together[0], limit.Together[1], limit.Together[2], limit.Together[3], limit.Together[4], limit.Together[5], limit.Together[6], limit.Flat)
		if limit.Idle != nil {
			fmt.Fprintf(b, "idle = %d\n", *limit.Idle)
		}
		if len(limit.Workload) > 0 {
			fmt.Fprint(b, "workload = {")
			separator := ""
			for _, workload := range slices.Sorted(maps.Keys(limit.Workload)) {
				fmt.Fprintf(b, "%s%s = %d", separator, strconv.Quote(workload), limit.Workload[workload])
				separator = ", "
			}
			fmt.Fprintln(b, "}")
		}
	}
	for _, joint := range cfg.Joints {
		fmt.Fprintf(b, "\n[[joint]]\nregimes = [\"R7\"]\nrate = %.17g\nafter_s = 0.0\nmembers = {", joint.Rate)
		separator := ""
		for core := range cfg.Cores {
			if offset, ok := joint.Members[core]; ok {
				fmt.Fprintf(b, "%s%s = %d", separator, strconv.Quote(strconv.Itoa(core)), offset)
				separator = ", "
			}
		}
		fmt.Fprintln(b, "}")
	}
}
