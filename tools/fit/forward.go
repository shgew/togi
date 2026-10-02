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
	starts, failures int
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

// starts contains only the decisive starts validated by decisive.
func forwardCheck(starts []trialfacts.Record) ([]forwardRow, forwardScore, error) {
	ordered := slices.Clone(starts)
	slices.SortStableFunc(ordered, func(a, b trialfacts.Record) int { return journal.CompareSessionIDs(a.Session, b.Session) })
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
			cfg, _ := fit(ordered[:begin])
			constant := float64(trainingFailures) / float64(begin)
			score, regimes, err := scoreForward(cfg, heldOut, seen, constant)
			if err != nil {
				return nil, forwardScore{}, fmt.Errorf("forward-chained session %s: %w", heldOut[0].Session, err)
			}
			rows = append(rows, forwardRow{session: heldOut[0].Session, ruleset: heldOut[0].Ruleset, trainingSessions: trainingSessions, constant: constant, score: score, regimes: regimes})
			pooled.starts += score.starts
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
		counts.starts++
		counts.predicted += p
		score.starts++
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

func reportForward(w io.Writer, rows []forwardRow, pooled forwardScore) {
	fmt.Fprintln(w, "\nForward-chained check")
	if len(rows) == 0 {
		fmt.Fprintln(w, "No held-out sessions (need at least two sessions with decisive starts).")
		return
	}
	for _, row := range rows {
		fmt.Fprintf(w, "%s ruleset=%d training_sessions=%d: ", row.session, row.ruleset, row.trainingSessions)
		reportForwardScore(w, row.score, fmt.Sprintf("%.3f", row.constant))
		for _, regime := range machine.Regimes {
			if counts, ok := row.regimes[regime]; ok {
				fmt.Fprintf(w, "  %s starts=%d observed=%d predicted=%.1f\n", regime, counts.starts, counts.failures, counts.predicted)
			}
		}
	}
	fmt.Fprint(w, "Pooled: ")
	reportForwardScore(w, pooled, "per-prefix")
}

func reportForwardScore(w io.Writer, score forwardScore, constant string) {
	n := float64(score.starts)
	fmt.Fprintf(w, "starts=%d failures=%d predicted=%.1f log_loss/start fit=%.4f constant(%s)=%.4f failures_p<0.01=%d exact_matches=%d/%d (%.1f%%) flagged/eligible=%d/%d\n", score.starts, score.failures, score.predicted, score.fitLoss/n, constant, score.constantLoss/n, score.surprises, score.matches, score.starts, 100*float64(score.matches)/n, score.flagged, score.eligible)
}
