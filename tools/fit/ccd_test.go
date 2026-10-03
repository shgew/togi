package main

import (
	"math"
	"testing"

	"github.com/shgew/togi/internal/facts"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
	"github.com/shgew/togi/tools/trialfacts"
)

func TestCCDPriorUsesLoadedExposureAndSmoothedOutcomes(t *testing.T) {
	records := []trialfacts.Record{
		{Profile: []int{-30, -30, -50, -50}, Class: facts.Class{Regime: machine.R7, Cores: []int{0, 1}, DurationS: 120}, Outcome: journal.OutcomeFailure},
		{Profile: []int{-30, -30, -50, -50}, Class: facts.Class{Regime: machine.R7, Cores: []int{0, 1}, DurationS: 120}, Outcome: journal.OutcomePass},
		{Profile: []int{-50, -50, 0, 0}, Class: facts.Class{Regime: machine.R2, Cores: []int{0}, DurationS: 900}, Outcome: journal.OutcomeFailure},
	}
	rate, depth, ok := ccdPrior(records)
	want := math.Log(math.Ln2 / 120)
	if !ok || math.Abs(rate-want) > 1e-12 || depth != 30 {
		t.Fatalf("prior rate=%g depth=%g support=%v; want %g, 30, true", rate, depth, ok, want)
	}
	if _, _, ok := ccdPrior(records[2:]); ok {
		t.Fatal("non-R7 exposure identified a prior")
	}
	records[0].Class.Cores, records[1].Class.Cores = []int{0, 2}, []int{0, 2}
	rate, depth, ok = ccdPrior(records[:2])
	if !ok || math.Abs(rate-(want-math.Ln2)) > 1e-12 || depth != 40 {
		t.Fatalf("two loaded CCDs: rate=%g depth=%g support=%v", rate, depth, ok)
	}
}
