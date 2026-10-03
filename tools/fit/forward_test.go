package main

import (
	"math"
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"
	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/internal/sim"
	"github.com/shgew/togi/tools/trialfacts"
)

func TestForwardCheck(t *testing.T) {
	record := trialfacts.Record{Kind: facts.TrialFact, Class: facts.Class{Regime: machine.R1, Workload: "fixture", Cores: []int{0}, DurationS: 1}, Profile: []int{-20, 0}, Outcome: journal.OutcomePass}
	t.Run("no leakage", func(t *testing.T) {
		var starts []trialfacts.Record
		// Deliberately put the latest session first, preserving within-session order.
		for session := 3; session >= 1; session-- {
			n := 3
			if session == 1 {
				n = 4
			}
			for i := range n {
				r := record
				r.Session = []string{"", "20260101T000000Z", "20260102T000000Z", "20260103T000000Z"}[session]
				r.Ruleset, r.Seq = session, i+1
				if i == 0 {
					r.Outcome = journal.OutcomeFailure
				}
				starts = append(starts, r)
			}
		}
		rows, pooled, err := forwardCheck(starts, 0)
		if err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff([]string{"20260102T000000Z", "20260103T000000Z"}, []string{rows[0].session, rows[1].session}); diff != "" {
			t.Fatal(diff)
		}
		if diff := cmp.Diff([]int{1, 2, 2, 3, 6, 6}, []int{rows[0].trainingSessions, rows[1].trainingSessions, rows[0].ruleset, rows[1].ruleset, pooled.starts, pooled.matches}); diff != "" {
			t.Fatal(diff)
		}
		if diff := cmp.Diff(0.25, rows[0].constant); diff != "" {
			t.Fatal(diff)
		}
		later := slices.Clone(starts)
		for i := range later {
			if later[i].Session == "20260103T000000Z" {
				later[i].Outcome = journal.OutcomeFailure
			}
		}
		changed, _, err := forwardCheck(later, 0)
		if err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff(rows[0], changed[0], cmp.AllowUnexported(forwardRow{}, forwardScore{}, forwardCounts{})); diff != "" {
			t.Fatalf("later outcomes changed the earlier held-out row (-want +got):\n%s", diff)
		}
		for i := range later {
			if later[i].Session == "20260102T000000Z" {
				later[i].Outcome = journal.OutcomeFailure
			}
		}
		changed, _, err = forwardCheck(later, 0)
		if err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff(rows[0].score.predicted, changed[0].score.predicted); diff != "" {
			t.Fatalf("held-out outcomes changed their own prediction (-want +got):\n%s", diff)
		}
	})
	t.Run("numeric session suffixes", func(t *testing.T) {
		sessions := []string{"20260101T000000Z", "20260101T000000Z-2", "20260101T000000Z-9", "20260101T000000Z-10"}
		var starts []trialfacts.Record
		for _, session := range slices.Backward(sessions) {
			r := record
			r.Session, r.Seq = session, 1
			starts = append(starts, r)
		}
		rows, _, err := forwardCheck(starts, 0)
		if err != nil {
			t.Fatal(err)
		}
		var got []string
		for _, row := range rows {
			got = append(got, row.session)
		}
		if diff := cmp.Diff(sessions[1:], got); diff != "" {
			t.Fatalf("held-out sessions out of chronological order (-want +got):\n%s", diff)
		}
		starts[0].Outcome = journal.OutcomeFailure
		changed, _, err := forwardCheck(starts, 0)
		if err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff(rows[:2], changed[:2], cmp.AllowUnexported(forwardRow{}, forwardScore{}, forwardCounts{})); diff != "" {
			t.Fatalf("later suffixed session changed earlier rows (-want +got):\n%s", diff)
		}
	})
	t.Run("seal", func(t *testing.T) {
		var starts []trialfacts.Record
		for i, session := range []string{"20260101T000000Z", "20260102T000000Z", "20260103T000000Z", "20260104T000000Z"} {
			r := record
			r.Session, r.Seq = session, 1
			if i%2 == 1 {
				r.Outcome = journal.OutcomeFailure
			}
			starts = append(starts, r, r)
		}
		all, _, err := forwardCheck(starts, 0)
		if err != nil {
			t.Fatal(err)
		}
		sealed, pooled, err := forwardCheck(starts, 1)
		if err != nil {
			t.Fatal(err)
		}
		if diff := cmp.Diff(all[:2], sealed, cmp.AllowUnexported(forwardRow{}, forwardScore{}, forwardCounts{})); diff != "" {
			t.Fatalf("sealing the newest session changed the earlier rows (-want +got):\n%s", diff)
		}
		if diff := cmp.Diff([]int{4, 2}, []int{pooled.starts, pooled.failures}); diff != "" {
			t.Fatalf("pooled score included the sealed session (-want +got):\n%s", diff)
		}
		if _, _, err := forwardCheck(starts, 3); err == nil || err.Error() != "--seal 3 leaves no held-out session to score (3 held out)" {
			t.Fatalf("sealing every held-out session: %v", err)
		}
	})
	t.Run("log loss and constant", func(t *testing.T) {
		model := sim.DefaultModel()
		model.PastLimitRate, model.NearLimitRate, model.OnsetBoost = 0, 0, 0
		cfg := sim.Config{Cores: 2, Model: &model, Limits: []sim.Limits{{Flat: math.Log(2)}, {}}}
		heldOut := []trialfacts.Record{record, record, record}
		for i := range heldOut {
			heldOut[i].Profile = []int{-1, 0}
			if i < 2 {
				heldOut[i].Outcome = journal.OutcomeFailure
			}
		}
		heldOut[2].Profile = []int{-2, 0}
		seen := map[string]bool{profileClassKey(heldOut[0]): true}
		score, regimes, err := scoreForward(cfg, heldOut, seen, 0.25)
		if err != nil {
			t.Fatal(err)
		}
		// Three one-second starts have p=1-exp(-log(2))=0.5, regardless
		// of measured duration. The constant assigns p=1/4 to two failures.
		counts := forwardCounts{starts: 3, failures: 2, predicted: 1.5}
		want := forwardScore{forwardCounts: counts, fitLoss: 3 * math.Log(2), constantLoss: -2*math.Log(0.25) - math.Log(0.75), matches: 2}
		opts := []cmp.Option{cmp.AllowUnexported(forwardScore{}, forwardCounts{}), cmpopts.EquateApprox(0, 1e-12)}
		if diff := cmp.Diff(want, score, opts...); diff != "" {
			t.Fatal(diff)
		}
		if diff := cmp.Diff(map[machine.Regime]forwardCounts{machine.R1: counts}, regimes, opts...); diff != "" {
			t.Fatal(diff)
		}
		for _, tc := range []struct {
			p       float64
			failure bool
			want    float64
		}{{0, true, -math.Log(1e-4)}, {1, false, -math.Log(1e-4)}, {0, false, -math.Log(1 - 1e-4)}, {1, true, -math.Log(1 - 1e-4)}} {
			if diff := cmp.Diff(tc.want, logLoss(tc.p, tc.failure), cmpopts.EquateApprox(0, 1e-12)); diff != "" {
				t.Error(diff)
			}
		}
	})
}
