package tuner

import (
	"fmt"
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func partialEvidence(h *harness, carried bool, outcome journal.Outcome, cores, profile []int, count int) {
	h.t.Helper()
	for range count {
		if carried {
			h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "old", Seq: len(h.events) + 1, Trial: "partial"}, Class: journal.TrialClass{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: cores, DurationS: 120}, Profile: profile, Condition: machine.Resident, Phase: journal.PhaseGuard, RecordOnly: true, Outcome: outcome, Signal: machine.ComputationError, Core: new(cores[0]), DurationS: 7})
		} else {
			h.trial(Action{Kind: RunTrial, Trial: Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: cores, DurationS: 120, Profile: profile, Condition: machine.Resident, Phase: journal.PhaseGuard, RecordOnly: true}}, journal.TrialEnd{Outcome: outcome, Signal: machine.ComputationError, Core: new(cores[0]), DurationS: 7})
		}
	}
}

func sevenCorePartialHarness(t *testing.T) (*harness, []int, []int) {
	t.Helper()
	profile := []int{-10, -20, -20, -20, -20, -20, -20, -20, -10, -20, -20, -20, -20, -20, -20, -20}
	return residentHarness(t, profile...), []int{1, 2, 3, 4, 5, 6, 7}, profile
}

func TestRecordOnlyEvidenceConsumers(t *testing.T) {
	for _, carried := range []bool{false, true} {
		for _, outcome := range []journal.Outcome{journal.OutcomePass, journal.OutcomeFailure} {
			t.Run(fmt.Sprintf("carried=%t/%s", carried, outcome), func(t *testing.T) {
				h, cores, profile := sevenCorePartialHarness(t)
				partialEvidence(h, carried, outcome, cores, profile, h.s.n)
				k := trialClass{machine.R7, machine.Workloads(machine.R7)[0].ID, coresKey(cores), 120}
				if len(h.s.ledger[k]) != 0 || len(h.s.failures) != 0 || len(h.s.pendingFailures) != 0 || len(h.s.queue) != 0 || len(h.s.obligations) != 0 || h.s.awaiting != nil || h.s.warning != nil {
					t.Fatal("record-only outcome entered decision evidence")
				}
				if _, ok := h.s.Drain(); ok {
					t.Fatal("record-only outcome requested a decision")
				}
				for _, rule := range []evidenceRule{allEvidence, edgeEvidence, huntEvidence, rerunEvidence, refinementEvidence, rotationEvidence} {
					if h.s.passes(k, profile, 0, rule) != 0 || h.s.fails(k, profile, 0) {
						t.Fatalf("record-only evidence admitted by rule %d", rule)
					}
				}
				tr := Trial{Regime: k.regime, Workload: k.workload, Cores: cores, DurationS: k.duration, Condition: machine.Resident, Phase: journal.PhaseRefine, Profile: profile}
				if a := h.s.skipKnownFailure(Action{Kind: RunTrial, Trial: tr}); a.Kind != RunTrial {
					t.Fatalf("record-only failure skipped a normal trial: %+v", a)
				}
				anchor := slices.Clone(profile)
				for _, id := range cores {
					anchor[id] = 0
				}
				h.add(&journal.HuntStart{Hunt: 1, Regime: k.regime, Workload: k.workload, Cores: cores, DurationS: 120, StartS: 120, Starts: h.s.n, Failing: profile, Anchor: anchor, Candidates: cores})
				for _, stage := range []string{"part", "complement", "full", "edge"} {
					a := h.s.planMask(h.s.hunt, maskPlan{cores: cores, set: cores, stage: stage, duration: 120}, "probe")
					mask := a.Payload.(*journal.HuntMask)
					if mask.Inferred != "" {
						t.Fatalf("%s inferred record-only outcome: %+v", stage, mask)
					}
					h.decide(a)
					if got := h.s.maskOutcome(h.s.hunt, h.s.hunt.masks[len(h.s.hunt.masks)-1]); got != "running" {
						t.Fatalf("%s running outcome %s", stage, got)
					}
					state := h.s.projectHunt()
					if got := state.Masks[len(state.Masks)-1].Passes; got != 0 {
						t.Fatalf("hunt projection counted %d record-only passes", got)
					}
				}
				assertProjectionReplay(h)
			})
		}
	}
}

func TestRecordOnlyDoesNotInvalidateOrSupplyMonotonicityEvidence(t *testing.T) {
	for _, carried := range []bool{false, true} {
		t.Run(fmt.Sprintf("carried=%t", carried), func(t *testing.T) {
			h, cores, profile := sevenCorePartialHarness(t)
			tr := Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: cores, DurationS: 120, Condition: machine.Masked, Phase: journal.PhaseHunt, Profile: profile}
			for range h.s.n {
				h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
			}
			partialEvidence(h, carried, journal.OutcomeFailure, cores, profile, 1)
			k := classOf(h.s.intents["0001"])
			if h.s.passes(k, profile, 0, allEvidence) != h.s.n || h.s.warning != nil {
				t.Fatal("partial failure contradicted normal passes or produced a warning")
			}
			h, cores, profile = sevenCorePartialHarness(t)
			partialEvidence(h, carried, journal.OutcomePass, cores, profile, h.s.n)
			h.trial(Action{Kind: RunTrial, Trial: tr}, failed)
			if h.s.warning != nil {
				t.Fatal("normal failure contradicted record-only passes")
			}
		})
	}
}

func TestRecordOnlySkipsNeitherKnownFailuresNorReruns(t *testing.T) {
	h, cores, profile := sevenCorePartialHarness(t)
	tr := Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: cores, DurationS: 120, Condition: machine.Resident, Phase: journal.PhaseGuard, Profile: profile}
	failure := h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "old", Trial: "normal"}, Class: journal.TrialClass{Regime: tr.Regime, Workload: tr.Workload, Cores: cores, DurationS: 120}, Condition: machine.Resident, Profile: profile, Outcome: journal.OutcomeFailure, Signal: machine.Crash})
	partialEvidence(h, false, journal.OutcomePass, cores, profile, h.s.n)
	if a := h.s.skipKnownFailure(Action{Kind: RunTrial, Trial: tr}); a.Kind != Decide {
		t.Fatal("record-only passes covered a normal known failure")
	}
	tr.RecordOnly = true
	if a := h.s.skipKnownFailure(Action{Kind: RunTrial, Trial: tr}); a.Kind != RunTrial {
		t.Fatal("known failure skipped a record-only start")
	}
	h.s.obligations = []rerun{{class: trialClass{tr.Regime, tr.Workload, coresKey(cores), 120}, seq: failure.Seq}}
	if _, pending := h.s.pendingRerun(); !pending {
		t.Fatal("record-only passes fulfilled a rerun")
	}
}

func TestRecordOnlyTrialIDDoesNotHideCarriedKnownFailure(t *testing.T) {
	h, cores, profile := sevenCorePartialHarness(t)
	tr := Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: cores, DurationS: 120, Condition: machine.Resident, Phase: journal.PhaseGuard, Profile: profile}
	carried := h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "old", Trial: "0001"}, Class: journal.TrialClass{Regime: tr.Regime, Workload: tr.Workload, Cores: cores, DurationS: tr.DurationS}, Condition: tr.Condition, Profile: profile, Outcome: journal.OutcomeFailure, Signal: machine.Crash})
	partialEvidence(h, false, journal.OutcomePass, cores, profile, 1)
	a := h.s.skipKnownFailure(Action{Kind: RunTrial, Trial: tr})
	if a.Kind != Decide {
		t.Fatal("ordinary trial did not skip the carried known failure")
	}
	failure, ok := a.Payload.(*journal.Failure)
	if !ok || failure.Trial != "0001" || failure.KnownFailure != carried.Seq {
		t.Fatalf("skip payload = %+v, want carried failure with colliding trial ID", a.Payload)
	}
	before := len(h.s.pendingFailures)
	h.decide(a)
	if len(h.s.pendingFailures) != before+1 {
		t.Fatal("local record-only trial ID hid an ordinary carried known-failure decision")
	}
	if got := h.s.pendingFailures[before].seq; got != carried.Seq {
		t.Fatalf("pending failure source = %d, want carried fact %d", got, carried.Seq)
	}
}

func TestRecordOnlyCannotFulfillRefinementChecks(t *testing.T) {
	for _, carried := range []bool{false, true} {
		for _, outcome := range []journal.Outcome{journal.OutcomePass, journal.OutcomeFailure} {
			t.Run(fmt.Sprintf("carried=%t/%s", carried, outcome), func(t *testing.T) {
				h, cores, profile := sevenCorePartialHarness(t)
				h.s.parts = [][]int{cores}
				h.add(&journal.RefineRound{Round: 1, Event: journal.RotationStart, Profile: profile, Target: profile, Cores: []int{1}, Starts: h.s.n, StartS: 120})
				h.s.round.initial = slices.Clone(profile)
				h.s.round.initial[1]++
				for _, r := range []machine.Regime{machine.R1, machine.R2} {
					for range h.s.n {
						h.trial(Action{Kind: RunTrial, Trial: Trial{Core: 1, Regime: r, Workload: machine.Workloads(r)[0].ID, DurationS: 120, Condition: machine.Resident, Phase: journal.PhaseRefine, Profile: profile}}, passed)
					}
				}
				partialEvidence(h, carried, outcome, cores, profile, h.s.n)
				a := h.s.skipKnownFailure(h.s.roundCheck())
				if a.Kind != RunTrial || a.Trial.Regime != machine.R7 || a.Trial.RecordOnly || !slices.Equal(a.Trial.Cores, cores) {
					t.Fatalf("record-only evidence answered refinement: %+v", a)
				}
				checks := h.s.projectRound().Checks
				if checks[len(checks)-1].Passes != 0 {
					t.Fatal("refinement projection counted partial evidence")
				}
			})
		}
	}
}

func TestR7PartialMasksFreezeAtStepStart(t *testing.T) {
	h := residentHarness(t, -10, -10, -20, -30, -40, -20, -20, -40)
	h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationStart, Steps: []machine.Regime{machine.R7}})
	a := h.next()
	step, ok := a.Payload.(*journal.GuardStep)
	if !ok {
		t.Fatalf("missing step snapshot: %+v", a)
	}
	want := []journal.GuardPartial{{CCD: 0, Cores: []int{2, 3}}, {CCD: 1, Cores: []int{4, 7}}}
	if diff := cmp.Diff(want, step.Partials); diff != "" {
		t.Fatal(diff)
	}
	h.decide(a)
	a = h.next()
	if !a.Trial.RecordOnly || !slices.Equal(a.Trial.Cores, []int{2, 3}) || a.Trial.Step != 1 {
		t.Fatalf("wrong first partial: %+v", a)
	}
	h.trial(a, failed)
	h.add(&journal.ProfileChange{From: step.Profile, To: []int{-25, -25, -20, -30, -40, -20, -20, -40}})
	replay := New()
	for _, e := range h.events {
		replay.Fold(e)
	}
	a = replay.rotationNext()
	if !a.Trial.RecordOnly || a.Trial.Retry || !slices.Equal(a.Trial.Cores, []int{2, 3}) {
		t.Fatalf("resume recomputed mask or retried failed start: %+v", a)
	}
	if got := replay.guard.partial[1].completed[classOf(h.s.intents["0001"])]; got != 1 {
		t.Fatalf("failed start not recorded as attempted: %d", got)
	}
}

func TestR7PartialFailuresLeaveQualificationAndProfileUnchanged(t *testing.T) {
	for _, topology := range []string{"two CCDs", "one CCD", "all tied"} {
		t.Run(topology, func(t *testing.T) {
			h := residentHarness(t, -10, -20, -10, -20)
			switch topology {
			case "one CCD":
				h.s.ccd = map[int]int{0: 0, 1: 0, 2: 0, 3: 0}
				h.s.parts = [][]int{{0, 1, 2, 3}}
			case "all tied":
				h.add(&journal.ProfileChange{To: []int{-20, -20, -20, -20}})
			}
			before := h.s.offsets()
			h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationStart, Steps: h.s.steps})
			partialStarts, snapshots := 0, 0
			for range 1000 {
				a := h.s.rotationNext()
				if step, ok := a.Payload.(*journal.GuardStep); ok {
					snapshots++
					if topology == "all tied" {
						for _, part := range step.Partials {
							if len(part.Cores) != 0 || !strings.Contains(part.Reason, "no cores to load") {
								t.Fatalf("empty partial has no skip reason: %+v", part)
							}
						}
					}
					h.decide(a)
					continue
				}
				if a.Kind == RunTrial {
					end := passed
					if a.Trial.RecordOnly {
						partialStarts++
						end = journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(a.Trial.Cores[0]), DurationS: 3}
					}
					h.trial(a, end)
					if h.s.awaiting != nil || len(h.s.queue) != 0 || h.s.hunt != nil || len(h.s.obligations) != 0 || h.s.warning != nil {
						t.Fatal("partial failure triggered a decision")
					}
					continue
				}
				end, ok := a.Payload.(*journal.GuardRotation)
				if !ok || !end.Clean || !end.Qualifying {
					t.Fatalf("partial failure dirtied rotation: %+v", a)
				}
				h.decide(a)
				if h.s.QualifiedRotations() != 1 || !slices.Equal(before, h.s.offsets()) || len(h.s.marks) != 0 {
					t.Fatal("partial failures changed profile, marks or qualification")
				}
				want := 24
				switch topology {
				case "one CCD":
					want = 12
				case "all tied":
					want = 0
				}
				if partialStarts != want || snapshots != 3 {
					t.Fatalf("partial starts %d want %d; snapshots %d want 3", partialStarts, want, snapshots)
				}
				return
			}
			t.Fatal("rotation did not finish")
		})
	}
}

func TestRecordOnlyDoesNotVetoLongHuntPrior(t *testing.T) {
	h, cores, profile := sevenCorePartialHarness(t)
	partialEvidence(h, false, journal.OutcomeFailure, cores, profile, 1)
	tr := Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: h.s.ids(), DurationS: 120, Condition: machine.Resident, Phase: journal.PhaseGuard, Profile: profile}
	for range h.s.n {
		h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
	}
	tr.DurationS = 600
	h.trial(Action{Kind: RunTrial, Trial: tr}, failed)
	failure := h.decide(h.next())
	h.decide(h.s.huntStartNext())
	plan, ok := h.s.nextMaskPlan(h.s.hunt)
	if !ok || plan.duration != 600 || !plan.escalated {
		t.Fatalf("partial short failure vetoed longer hunt: %+v, source #%d", plan, failure.Seq)
	}
}

func TestRecordOnlyCannotContradictQualifiedProfile(t *testing.T) {
	for _, carried := range []bool{false, true} {
		t.Run(fmt.Sprintf("carried=%t", carried), func(t *testing.T) {
			h, cores, profile := sevenCorePartialHarness(t)
			h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationStart, Steps: h.s.steps})
			h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationEnd, Clean: true, Qualifying: true})
			shallow := slices.Clone(profile)
			shallow[0]++
			h.add(&journal.ProfileChange{From: profile, To: shallow})
			h.add(&journal.ProfileChange{From: shallow, To: profile})
			partialEvidence(h, carried, journal.OutcomeFailure, cores, profile, 1)
			if h.s.QualifiedRotations() != 1 || h.s.covering() == 0 {
				t.Fatal("partial failure contradicted an earlier qualified rotation")
			}
			assertProjectionReplay(h)
		})
	}
}

func TestRecordOnlyInconclusiveRetriesSamePart(t *testing.T) {
	h := residentHarness(t, -10, -20, -10, -20)
	h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationStart, Steps: []machine.Regime{machine.R7}})
	h.decide(h.next())
	first := h.next()
	h.trial(first, unsure)
	retry := h.next()
	if !retry.Trial.RecordOnly || !retry.Trial.Retry || retry.Trial.Step != first.Trial.Step || retry.Trial.DurationS != first.Trial.DurationS || !slices.Equal(retry.Trial.Cores, first.Trial.Cores) {
		t.Fatalf("inconclusive partial lost its retry identity: %+v", retry)
	}
	h.trial(retry, failed)
	next := h.next()
	if !next.Trial.RecordOnly || next.Trial.Retry {
		t.Fatalf("failure retried rather than advancing: %+v", next)
	}
}
