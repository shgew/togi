// carry-facts prepares or simulates a transition in a temporary copy of recorded state, without hardware.
package main

import (
	"context"
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strings"

	"github.com/shgew/togi/internal/carry"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/session"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/internal/simrun"
	"github.com/shgew/togi/internal/tuner"
)

func main() {
	if err := run(os.Args[1:], os.Stdout, os.Stderr); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run(args []string, out, errOut io.Writer) (err error) {
	flags := flag.NewFlagSet("carry-facts", flag.ContinueOnError)
	flags.SetOutput(errOut)
	dirArg := flags.String("state-dir", "", "required temporary copy of the state directory; preparation mutates this copy")
	simulate := flags.Bool("simulate", false, "run the current ruleset through its first passed full cycle using the recorded BIOS context")
	seed := flags.Uint64("seed", 1, "draw simulated limits and outcomes from this seed")
	if err := flags.Parse(args); err != nil {
		return err
	}
	if *dirArg == "" || flags.NArg() != 0 {
		return fmt.Errorf("carry-facts: supply --state-dir with a temporary state copy and no positional arguments")
	}
	dir, err := filepath.EvalSymlinks(*dirArg)
	if err != nil {
		return fmt.Errorf("carry-facts: resolve state copy: %w", err)
	}
	dir, err = filepath.Abs(dir)
	if err != nil {
		return fmt.Errorf("carry-facts: resolve absolute state copy: %w", err)
	}
	if dir == "/var/lib/togi" || strings.HasPrefix(dir, "/var/lib/togi"+string(filepath.Separator)) {
		return fmt.Errorf("carry-facts: refusing the real state directory")
	}
	temp, err := filepath.EvalSymlinks(os.TempDir())
	if err != nil {
		return fmt.Errorf("carry-facts: resolve temporary directory: %w", err)
	}
	rel, err := filepath.Rel(temp, dir)
	if err != nil || rel == "." || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
		return fmt.Errorf("carry-facts: --state-dir must name a copy beneath %s", temp)
	}
	for _, name := range []string{"archive", "events.jsonl", "lock"} {
		info, err := os.Lstat(filepath.Join(dir, name))
		if os.IsNotExist(err) {
			continue
		}
		if err != nil {
			return fmt.Errorf("carry-facts: inspect %s: %w", name, err)
		}
		if info.Mode()&os.ModeSymlink != 0 {
			return fmt.Errorf("carry-facts: state copy must not symlink %s", name)
		}
	}
	live, err := facts.ReadJournal(filepath.Join(dir, "events.jsonl"))
	if err != nil {
		return fmt.Errorf("carry-facts: read live recorded facts: %w", err)
	}
	if live.Context == nil {
		return fmt.Errorf("carry-facts: live journal has no recorded BIOS context")
	}
	if *simulate {
		return simulateRecorded(dir, live, *seed, out)
	}
	build := journal.Build{Schema: journal.Schema, Ruleset: tuner.Ruleset + 1}
	j, err := journal.Lock(dir, journal.Options{Build: build})
	if err != nil {
		return fmt.Errorf("carry-facts: lock state copy: %w", err)
	}
	defer func() {
		if closeErr := j.Close(); err == nil && closeErr != nil {
			err = fmt.Errorf("carry-facts: close state copy: %w", closeErr)
		}
	}()
	prepared, err := carry.Prepare(j, build, nil, live.Context)
	if err != nil {
		return fmt.Errorf("carry-facts: prepare transition: %w", err)
	}
	if prepared == nil {
		return fmt.Errorf("carry-facts: state copy produced no pending transition")
	}
	type counts struct{ passes, failures int }
	bySource := map[string]counts{}
	for _, fact := range prepared.Facts {
		count := bySource[fact.Session]
		switch fact.Outcome {
		case journal.OutcomePass:
			count.passes++
		case journal.OutcomeFailure:
			count.failures++
		case journal.OutcomeInconclusive:
			continue
		}
		bySource[fact.Session] = count
	}
	ids := make([]string, 0, len(bySource))
	for id := range bySource {
		ids = append(ids, id)
	}
	slices.SortFunc(ids, journal.CompareSessionIDs)
	if _, err := fmt.Fprintf(out, "evidence epoch %d; prepared ruleset %d transition in temporary copy\nSOURCE SESSION       PASSES FAILURES\n", tuner.EvidenceEpoch, build.Ruleset); err != nil {
		return err
	}
	for _, id := range ids {
		count := bySource[id]
		if _, err := fmt.Fprintf(out, "%s %6d %8d\n", id, count.passes, count.failures); err != nil {
			return err
		}
	}
	return nil
}

func simulateRecorded(dir string, live facts.Session, seed uint64, out io.Writer) error {
	cfg, err := sim.Resume(dir, sim.Config{Seed: seed, Cores: len(live.Cores), BIOSContext: *live.Context})
	if err != nil {
		return fmt.Errorf("carry-facts: resume recorded state on simulator: %w", err)
	}
	m, err := sim.New(cfg)
	if err != nil {
		return fmt.Errorf("carry-facts: create simulator: %w", err)
	}
	stop, err := simrun.Simulate(context.Background(), simrun.Input{
		Config: config.Default(), ConfigPath: config.DefaultPath, Dir: dir, Machine: m,
		Until: func(e journal.Event) bool {
			p, ok := e.Data.(*journal.CheckingCycle)
			return ok && p.Event == journal.CycleEnd && p.Passed && p.Full
		},
	})
	if err != nil {
		return fmt.Errorf("carry-facts: simulate recorded transition: %w", err)
	}
	events, torn, err := journal.Read(dir)
	if err != nil {
		return fmt.Errorf("carry-facts: read simulated journal: %w", err)
	}
	if torn != nil {
		return fmt.Errorf("carry-facts: simulated journal has a torn tail")
	}
	if stop.Reason == session.StopDeadEnd {
		return fmt.Errorf("carry-facts: simulator dead end %s: %s", stop.DeadEnd.Condition, stop.DeadEnd.Detail)
	}
	return renderTransition(out, summarizeTransition(events))
}

type transitionDemo struct {
	passes               int
	failures             int
	answered             int
	soloLimitTrials      int
	cycleTrials          int
	cyclePasses          int
	firstTrials          []journal.Event
	decisions            []journal.Event
	firstPassedFullCycle *journal.Event
	skips                []journal.Event
	hunts                []journal.Event
	inferredGroups       []journal.Event
}

func summarizeTransition(events []journal.Event) transitionDemo {
	var passes, failures, answered, soloLimitTrials, cycleTrials, cyclePasses int
	checking := map[int]bool{}
	carried := map[int]bool{}
	cycleTrials := map[string]bool{}
	var firstTrials []journal.Event
	var decisions []journal.Event
	var firstPassedFullCycle *journal.Event
	var skips, hunts, inferredGroups []journal.Event
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.TrialCarried:
			carried[e.Seq] = true
			switch p.Outcome {
			case journal.OutcomePass:
				passes++
			case journal.OutcomeFailure:
				failures++
			case journal.OutcomeInconclusive:
			}
		case *journal.FailureCarried:
			carried[e.Seq] = true
			failures++
		case *journal.Failure:
			if p.KnownFailure != 0 {
				skips = append(skips, e)
			}
		case *journal.HuntStart:
			if carried[p.Failure] {
				hunts = append(hunts, e)
			}
		case *journal.HuntGroup:
			if p.Inferred != "" && citesCarriedFact(e.Cause, carried) {
				inferredGroups = append(inferredGroups, e)
			}
		case *journal.TunerDecision:
			checking[p.Core] = p.Decision == journal.CheckSoloLimit
		case *journal.CorePhase:
			if !p.CheckSoloLimit && p.From == journal.PhaseSearch && p.To != journal.PhaseSearch && checking[p.Core] {
				if citesCarriedFact(e.Cause, carried) {
					answered++
					decisions = append(decisions, e)
				}
			}
			checking[p.Core] = p.CheckSoloLimit
		case *journal.TrialIntent:
			if len(firstTrials) < 5 {
				firstTrials = append(firstTrials, e)
			}
			if p.Phase == journal.PhaseSearch && p.Core != nil && checking[*p.Core] {
				soloLimitTrials++
			}
			if p.Phase == journal.PhaseChecking && p.Cycle == 1 {
				cycleTrials[p.Trial] = true
				cycleTrials++
			}
		case *journal.TrialEnd:
			if cycleTrials[p.Trial] && p.Outcome == journal.OutcomePass {
				cyclePasses++
			}
		case *journal.CheckingCycle:
			if p.Event == journal.CycleEnd && p.Passed && p.Full && firstPassedFullCycle == nil {
				copy := e
				firstPassedFullCycle = &copy
			}
		}
	}
	return transitionDemo{
		passes: passes, failures: failures, answered: answered, soloLimitTrials: soloLimitTrials,
		cycleTrials: cycleTrials, cyclePasses: cyclePasses,
		firstTrials: firstTrials, decisions: decisions, firstPassedFullCycle: firstPassedFullCycle,
		skips: skips, hunts: hunts, inferredGroups: inferredGroups,
	}
}

func citesCarriedFact(cause []int, carried map[int]bool) bool {
	for _, seq := range cause {
		if carried[seq] {
			return true
		}
	}
	return false
}

func renderTransition(out io.Writer, demo transitionDemo) error {
	if _, err := fmt.Fprintf(out, "ruleset %d simulated with recorded BIOS context; evidence epoch %d\ncarried facts: %d passes, %d failures\ncandidate-solo-limit completions citing carried passes: %d\nlive solo-limit-check trials: %d\nfirst cycle live trials: %d; live passes: %d\n", session.Build().Ruleset, tuner.EvidenceEpoch, demo.passes, demo.failures, demo.answered, demo.soloLimitTrials, demo.cycleTrials, demo.cyclePasses); err != nil {
		return err
	}
	for _, e := range demo.decisions {
		if _, err := fmt.Fprintf(out, "carried solo-limit decision #%d cause=%v: %s\n", e.Seq, e.Cause, e.Msg); err != nil {
			return err
		}
	}
	for _, e := range demo.firstTrials {
		if _, err := fmt.Fprintf(out, "first live trial #%d: %s\n", e.Seq, e.Msg); err != nil {
			return err
		}
	}
	for _, e := range demo.skips {
		if _, err := fmt.Fprintf(out, "known-failure skip #%d cause=%v: %s\n", e.Seq, e.Cause, e.Msg); err != nil {
			return err
		}
	}
	for _, e := range demo.hunts {
		p := e.Data.(*journal.HuntStart)
		if _, err := fmt.Fprintf(out, "carried-failure hunt #%d cause=%v: source failure #%d trial %s; class %s %s cores=%v duration=%ds; failing=%v; %s\n", e.Seq, e.Cause, p.Failure, p.Trial, p.Regime, p.Workload, p.Cores, p.DurationS, p.Failing, e.Msg); err != nil {
			return err
		}
	}
	for _, e := range demo.inferredGroups {
		if _, err := fmt.Fprintf(out, "carried group inference #%d cause=%v: %s\n", e.Seq, e.Cause, e.Msg); err != nil {
			return err
		}
	}
	if demo.firstPassedFullCycle == nil {
		return fmt.Errorf("carry-facts: simulator stopped before a passed full cycle")
	}
	_, err := fmt.Fprintf(out, "first passed full cycle #%d: %s\n", demo.firstPassedFullCycle.Seq, demo.firstPassedFullCycle.Msg)
	return err
}
