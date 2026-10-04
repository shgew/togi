package main

import (
	"encoding/json"
	"fmt"
	"io"
	"math"
	"slices"
	"time"

	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/tools/modelcheck"
	"github.com/shgew/togi/tools/trialfacts"
)

type forwardCounts struct {
	trials, failures int
	predicted        float64
}

type forwardScore struct {
	forwardCounts
	fitLoss, constantLoss float64
	surprises, matches    int
	flagged, eligible     int
}

type forwardRow struct {
	session          string
	ruleset          int
	trainingSessions int
	constant         float64
	score            forwardScore
	regimes          map[machine.Regime]forwardCounts
}

func forward(extract string, seal, jobs int, stdout io.Writer) error {
	records, err := trialfacts.Read(extract)
	if err != nil {
		return err
	}
	trials, err := decisive(records)
	if err != nil {
		return err
	}
	return reportForwardCheck(stdout, trials, seal, jobs)
}

func reportForwardCheck(w io.Writer, trials []trialfacts.Record, seal, jobs int) error {
	started := time.Now()
	rows, pooled, err := forwardCheck(trials, seal, jobs)
	if err != nil {
		return err
	}
	reportForward(w, rows, pooled, seal)
	fmt.Fprintf(w, "Forward-chained elapsed: %s\n", time.Since(started).Round(time.Millisecond))
	return nil
}

// trials contains only the decisive trials validated by decisive. The newest
// seal sessions are neither fitted nor scored.
func forwardCheck(trials []trialfacts.Record, seal, jobs int) ([]forwardRow, forwardScore, error) {
	ordered := slices.Clone(trials)
	slices.SortStableFunc(ordered, func(a, b trialfacts.Record) int { return journal.CompareSessionIDs(a.Session, b.Session) })
	var bounds []int
	for i := range ordered {
		if i == 0 || ordered[i].Session != ordered[i-1].Session {
			bounds = append(bounds, i)
		}
	}
	if seal > 0 {
		if seal >= len(bounds)-1 {
			return nil, forwardScore{}, fmt.Errorf("--seal %d leaves no held-out session to score (%d held out)", seal, max(len(bounds)-1, 0))
		}
		ordered = ordered[:bounds[len(bounds)-seal]]
		bounds = bounds[:len(bounds)-seal]
	}
	configs, err := fitParallel(max(len(bounds)-1, 0), jobs, func(i int) (sim.Config, error) {
		cfg, _ := fit(ordered[:bounds[i+1]])
		return cfg, nil
	})
	if err != nil {
		return nil, forwardScore{}, err
	}
	seen := make(map[string]bool)
	trainingFailures, trainingSessions := 0, 0
	var rows []forwardRow
	var pooled forwardScore
	for begin := 0; begin < len(ordered); {
		end := begin + 1
		for end < len(ordered) && ordered[end].Session == ordered[begin].Session {
			end++
		}
		heldOut := ordered[begin:end]
		if begin > 0 {
			cfg := configs[trainingSessions-1]
			constant := float64(trainingFailures) / float64(begin)
			score, regimes, err := scoreForward(cfg, heldOut, seen, constant)
			if err != nil {
				return nil, forwardScore{}, fmt.Errorf("forward-chained session %s: %w", heldOut[0].Session, err)
			}
			rows = append(rows, forwardRow{session: heldOut[0].Session, ruleset: heldOut[0].Ruleset, trainingSessions: trainingSessions, constant: constant, score: score, regimes: regimes})
			pooled.trials += score.trials
			pooled.failures += score.failures
			pooled.predicted += score.predicted
			pooled.fitLoss += score.fitLoss
			pooled.constantLoss += score.constantLoss
			pooled.surprises += score.surprises
			pooled.matches += score.matches
			pooled.flagged += score.flagged
			pooled.eligible += score.eligible
		}
		// Publish this session to the training prefix only after scoring it.
		for _, r := range heldOut {
			seen[profileClassKey(r)] = true
			if r.Outcome == journal.OutcomeFailure {
				trainingFailures++
			}
		}
		trainingSessions++
		begin = end
	}
	return rows, pooled, nil
}

func profileClassKey(r trialfacts.Record) string {
	key, _ := json.Marshal(struct {
		Profile []int
		Class   facts.Class
	}{r.Profile, r.Class})
	return string(key)
}

func logLoss(p float64, failure bool) float64 {
	p = min(max(p, 1e-4), 1-1e-4)
	if failure {
		return -math.Log(p)
	}
	return -math.Log(1 - p)
}

func scoreForward(cfg sim.Config, heldOut []trialfacts.Record, seen map[string]bool, constant float64) (forwardScore, map[machine.Regime]forwardCounts, error) {
	m, err := sim.New(cfg)
	if err != nil {
		return forwardScore{}, nil, err
	}
	checker, err := modelcheck.NewChecker(cfg, heldOut)
	if err != nil {
		return forwardScore{}, nil, err
	}
	check := checker.Check("", "", m)
	score := forwardScore{eligible: len(check.Groups)}
	for _, group := range check.Groups {
		if group.Flagged {
			score.flagged++
		}
	}
	regimes := make(map[machine.Regime]forwardCounts)
	for _, r := range heldOut {
		spec := machine.TrialSpec{Regime: r.Class.Regime, Workload: machine.Workload{ID: r.Class.Workload}, Cores: r.Class.Cores, Duration: time.Duration(r.Class.DurationS) * time.Second, Condition: r.Condition}
		p := m.FailureProbability(r.Profile, spec)
		failure := r.Outcome == journal.OutcomeFailure
		counts := regimes[r.Class.Regime]
		counts.trials++
		counts.predicted += p
		score.trials++
		score.predicted += p
		if failure {
			counts.failures++
			score.failures++
			if p < 0.01 {
				score.surprises++
			}
		}
		regimes[r.Class.Regime] = counts
		score.fitLoss += logLoss(p, failure)
		score.constantLoss += logLoss(constant, failure)
		if seen[profileClassKey(r)] {
			score.matches++
		}
	}
	return score, regimes, nil
}

func reportForward(w io.Writer, rows []forwardRow, pooled forwardScore, seal int) {
	fmt.Fprintln(w, "\nForward-chained check")
	if len(rows) == 0 {
		fmt.Fprintln(w, "No held-out sessions (need at least two sessions with decisive trials).")
		return
	}
	for _, row := range rows {
		fmt.Fprintf(w, "%s ruleset=%d training_sessions=%d: ", row.session, row.ruleset, row.trainingSessions)
		reportForwardScore(w, row.score, fmt.Sprintf("%.3f", row.constant))
		for _, regime := range machine.Regimes {
			if counts, ok := row.regimes[regime]; ok {
				fmt.Fprintf(w, "  %s trials=%d observed=%d predicted=%.1f\n", regime, counts.trials, counts.failures, counts.predicted)
			}
		}
	}
	if seal > 0 {
		fmt.Fprintf(w, "Sealed newest sessions: %d (neither fitted nor scored)\n", seal)
	}
	fmt.Fprint(w, "Pooled: ")
	reportForwardScore(w, pooled, "per-prefix")
}

func reportForwardScore(w io.Writer, score forwardScore, constant string) {
	n := float64(score.trials)
	fmt.Fprintf(w, "trials=%d failures=%d predicted=%.1f log_loss/trial fit=%.4f constant(%s)=%.4f failures_p<0.01=%d exact_matches=%d/%d (%.1f%%) flagged/eligible=%d/%d\n", score.trials, score.failures, score.predicted, score.fitLoss/n, constant, score.constantLoss/n, score.surprises, score.matches, score.trials, 100*float64(score.matches)/n, score.flagged, score.eligible)
}
