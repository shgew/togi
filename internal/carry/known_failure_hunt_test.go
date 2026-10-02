package carry

import (
	"fmt"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/defect"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestPrepareCarriesHuntCulpritFromKnownFailure(t *testing.T) {
	for _, kind := range []string{"live trial", "carried trial", "carried idle"} {
		for _, reset := range []bool{false, true} {
			t.Run(fmt.Sprintf("%s/reset=%t", kind, reset), func(t *testing.T) {
				dir := t.TempDir()
				cores := []machine.CoreInfo{{Core: 5}, {Core: 0}, {Core: 2}}
				w := newJournal(t, dir, "X", 7, &context, cores...)
				w.add(&journal.SessionCarried{Sources: []journal.CarriedSource{src("Y", 6)}, Marks: true})
				profile := []int{-10, -35, -20}
				class := journal.TrialClass{Regime: machine.R7, Workload: "load", Cores: []int{0, 2, 5}, DurationS: 120}
				trial, signal := "origin", machine.ComputationError
				var failure int
				switch kind {
				case "live trial":
					w.add(&journal.TrialIntent{Trial: trial, Regime: class.Regime, Workload: class.Workload, Cores: class.Cores, DurationS: class.DurationS, Condition: machine.Resident, Profile: profile})
					failure = w.add(&journal.TrialEnd{Trial: trial, Outcome: journal.OutcomeFailure, Signal: signal})
					w.add(&journal.Failure{Trial: trial, Signal: signal, Attribution: journal.Unattributed, Regime: class.Regime, Condition: machine.Resident, Profile: profile}, failure)
				case "carried trial":
					signal = machine.Stall
					failure = w.add(&journal.TrialCarried{Source: journal.FactSource{Session: "Y", Seq: 4, Trial: trial}, Class: class, Condition: machine.Resident, Profile: profile, Outcome: journal.OutcomeFailure, Signal: signal})
				case "carried idle":
					trial, signal = "", machine.Crash
					class.Regime, class.Workload, class.DurationS = machine.R6, "", 0
					failure = w.add(&journal.FailureCarried{Source: journal.FactSource{Session: "Y", Seq: 4}, Class: class, Signal: signal, Attribution: journal.Unattributed, Regime: class.Regime, Condition: machine.Resident, Profile: profile})
				}
				skip := w.add(&journal.Failure{KnownFailure: failure, Trial: trial, Signal: signal, Attribution: journal.Unattributed, Regime: class.Regime, Condition: machine.Resident, Profile: profile}, failure)
				w.add(&journal.HuntStart{Hunt: 1, Failure: failure, Trial: trial, Regime: class.Regime, Workload: class.Workload, Cores: class.Cores, DurationS: class.DurationS, Failing: profile, Candidates: []int{2}}, failure, skip)
				end := w.add(&journal.HuntEnd{Hunt: 1, Result: "culprit", Cores: []int{2}})
				if reset {
					w.add(&journal.CommandReset{Core: new(2)})
				}
				w.close()
				j, err := journal.Lock(dir, opts())
				if err != nil {
					t.Fatal(err)
				}
				defer j.Close()
				got, err := Prepare(j, journal.Build{Schema: 2, Ruleset: 8}, []defect.Entry{}, nil)
				if err != nil {
					t.Fatal(err)
				}
				if got == nil {
					t.Fatal("next transition did not prepare carry")
				}
				var want []journal.CarriedCore
				if !reset {
					want = []journal.CarriedCore{{Core: 2, FailedMark: new(-35), MarkSession: "X", MarkSeq: end, MarkSignal: signal}}
				}
				if diff := cmp.Diff(want, got.Cores); diff != "" {
					t.Fatalf("culprit carry (-want +got):\n%s", diff)
				}
			})
		}
	}
}
