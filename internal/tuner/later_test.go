package tuner

import (
	"fmt"
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func startCycle(h *harness, n int) journal.Event {
	return h.add(&journal.CheckingCycle{Cycle: n, Event: journal.CycleStart, Steps: []machine.Regime{machine.R1}})
}

func endCycle(h *harness, n int) journal.Event {
	return h.add(&journal.CheckingCycle{Cycle: n, Event: journal.CycleEnd, Passed: true, Full: true})
}

// passCycles runs bare passed full cycles numbered from..to.
func passCycles(h *harness, from, to int) journal.Event {
	var end journal.Event
	for n := from; n <= to; n++ {
		startCycle(h, n)
		end = endCycle(h, n)
	}
	return end
}

// laterHarness concludes phase 2 with two bare cycles (phase 1's and the confirmation) and passes a third, so the
// profile has passed a cycle after the conclusion and the next failure is a later one.
func laterHarness(t *testing.T, offsets ...int) *harness {
	t.Helper()
	h := hasRoomHarness(t, offsets...)
	passCycles(h, 1, 3)
	if !h.s.laterGate() {
		t.Fatalf("later-failure gate closed after a passed cycle after the conclusion: %+v", h.s.phases)
	}
	return h
}

// failCore fails a single-core together trial of core id at its offset and records the attribution.
func failCore(h *harness, id int) journal.Event {
	h.t.Helper()
	tr := Trial{Regime: machine.R1, Workload: machine.Workloads(machine.R1)[0].ID, Condition: machine.Together, Phase: journal.PhaseChecking, DurationS: 120, Core: id, Offset: h.s.core(id).offset, Cycle: h.s.checking.cycle}
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(id), DurationS: 41})
	a := h.next()
	if f, ok := a.Payload.(*journal.Failure); !ok || f.Attribution != journal.Attributed || *f.Core != id {
		h.t.Fatalf("attribution %+v", a)
	}
	return h.decide(a)
}

func nextDecision(h *harness) (*journal.TunerDecision, Action) {
	h.t.Helper()
	a := h.next()
	d, ok := a.Payload.(*journal.TunerDecision)
	if !ok {
		h.t.Fatalf("want a tuner decision, got %+v", a)
	}
	return d, a
}

func isHold(d *journal.TunerDecision) bool {
	return d.Phase == journal.PhaseChecking && d.Decision == journal.Backoff && d.FromOffset == d.ToOffset && strings.HasPrefix(d.Reason, "holds")
}

// settle decides whatever the tuner asks until it wants a trial again.
func settle(h *harness) {
	h.t.Helper()
	for range 40 {
		a := h.next()
		if a.Kind != Decide {
			return
		}
		h.decide(a)
	}
	h.t.Fatal("tuner kept deciding")
}

func assertLaterReplay(h *harness) {
	h.t.Helper()
	replayed := replayState(h.events)
	if diff := cmp.Diff(h.s.Next(), replayed.Next()); diff != "" {
		h.t.Fatalf("next after replay (-live +replayed):\n%s", diff)
	}
	if diff := cmp.Diff(h.s.BIOSProfile(), replayed.BIOSProfile()); diff != "" {
		h.t.Fatalf("BIOS profile after replay (-live +replayed):\n%s", diff)
	}
	if diff := cmp.Diff(h.s.later.strikes, replayed.later.strikes, cmp.AllowUnexported(strike{})); diff != "" {
		h.t.Fatalf("strikes after replay (-live +replayed):\n%s", diff)
	}
	assertProjectionReplay(h)
}

func TestLaterGateClosedStepsBackAtOnce(t *testing.T) {
	for _, tt := range []struct {
		name   string
		cycles int
	}{{"before phase 1 ends", 0}, {"in the confirmation", 1}, {"first cycle after the conclusion", 2}} {
		t.Run(tt.name, func(t *testing.T) {
			h := hasRoomHarness(t, -10, -12)
			passCycles(h, 1, tt.cycles)
			startCycle(h, tt.cycles+1)
			f := failCore(h, 0)
			d, _ := nextDecision(h)
			if d.FromOffset != -10 || d.ToOffset != -9 || isHold(d) || strings.Contains(d.Reason, "second failure") {
				t.Fatalf("ordinary step-back expected: %+v", d)
			}
			h.decide(h.next())
			if diff := cmp.Diff([]int{f.Seq}, h.events[len(h.events)-1].Cause); diff != "" {
				t.Fatalf("cause (-want +got):\n%s", diff)
			}
			assertLaterReplay(h)
		})
	}
}

func TestLaterFirstFailureHoldsAndSecondStepsBackNamingBoth(t *testing.T) {
	h := laterHarness(t, -10, -12)
	startCycle(h, 4)
	first := failCore(h, 0)
	hold, a := nextDecision(h)
	if !isHold(hold) || hold.Core != 0 || hold.FromOffset != -10 || hold.ToOffset != -10 || hold.Pass == nil || *hold.Pass != -10 || hold.FailurePoint == nil || *hold.FailurePoint != -11 {
		t.Fatalf("hold %+v", hold)
	}
	if missing := missingTokens(hold.Reason, fmt.Sprintf("#%d", first.Seq), "cycle 4", fmt.Sprintf("cycle %d", 4+laterWindow-1), fmt.Sprintf("K = %d", laterWindow), "core 00"); len(missing) > 0 {
		t.Fatalf("hold reason %q lacks %q", hold.Reason, missing)
	}
	if a.Cause[0] != first.Seq {
		t.Fatalf("hold cause %v, want failure #%d first", a.Cause, first.Seq)
	}
	h.decide(a)
	if len(h.s.obligations) != 0 || h.s.core(0).offset != -10 || *h.s.core(0).fail != -11 {
		t.Fatalf("a hold moved something: obligations %v, core %+v", h.s.obligations, h.s.core(0))
	}
	if bios := h.s.BIOSProfile(); !slices.Equal(bios.Offsets, []int{-9, -12}) || !slices.Equal(bios.Unconfirmed, []int{0}) || bios.Since != h.events[len(h.events)-1].Seq {
		t.Fatalf("a held failure shows its stepped-back offset unconfirmed: %+v", bios)
	}
	if next := h.next(); next.Kind != RunTrial || next.Trial.Rerun {
		t.Fatalf("a held failure must be retested in its cycle, not skipped as a known failure: %+v", next)
	}
	assertLaterReplay(h)

	second := failCore(h, 0)
	step, a := nextDecision(h)
	if isHold(step) || step.FromOffset != -10 || step.ToOffset != -9 {
		t.Fatalf("second failure %+v", step)
	}
	if diff := cmp.Diff([]int{second.Seq, first.Seq}, a.Cause); diff != "" {
		t.Fatalf("pair cause (-want +got):\n%s", diff)
	}
	if missing := missingTokens(step.Reason, fmt.Sprintf("#%d", first.Seq), fmt.Sprintf("#%d", second.Seq), fmt.Sprintf("within %d cycles", laterWindow)); len(missing) > 0 {
		t.Fatalf("pair reason %q lacks %q", step.Reason, missing)
	}
	decision := h.decide(a)
	if len(h.s.obligations) != 1 || h.s.obligations[0].seq != second.Seq {
		t.Fatalf("rerun obligations %+v answer the second failure #%d only", h.s.obligations, second.Seq)
	}
	if bios := h.s.BIOSProfile(); !slices.Equal(bios.Offsets, []int{-9, -12}) || !slices.Equal(bios.Unconfirmed, []int{0}) || bios.Since != decision.Seq {
		t.Fatalf("stepped-back profile must replace the BIOS profile at the decision: %+v", bios)
	}
	if len(h.s.later.strikes) != 0 {
		t.Fatalf("a paired strike stayed: %+v", h.s.later.strikes)
	}
	settle(h)
	if h.s.checking.profile[0] != -9 {
		t.Fatalf("profile change not applied: %v", h.s.checking.profile)
	}
	end := endCycle(h, 4)
	if bios := h.s.BIOSProfile(); len(bios.Unconfirmed) != 0 || bios.Since != 0 || bios.Confirmed != end.Seq || !slices.Equal(bios.Offsets, []int{-9, -12}) {
		t.Fatalf("a passed cycle must confirm the stepped-back profile: %+v", bios)
	}
	assertLaterReplay(h)
}

func TestLaterHoldKeepsShowingItsStepBackAfterAPassedCycle(t *testing.T) {
	h := laterHarness(t, -10, -12)
	startCycle(h, 4)
	failCore(h, 0)
	h.decide(h.next())
	end := endCycle(h, 4)
	bios := h.s.BIOSProfile()
	if bios.Confirmed != end.Seq || !slices.Equal(bios.Offsets, []int{-9, -12}) || !slices.Equal(bios.Unconfirmed, []int{0}) {
		t.Fatalf("a valid hold keeps showing its step-back after a passed cycle: %+v", bios)
	}
	if len(h.s.later.strikes) != 1 {
		t.Fatalf("the strike must outlive passed cycles inside its window: %+v", h.s.later.strikes)
	}
	assertLaterReplay(h)
}

func TestLaterFailuresOnDifferentCoresHoldSeparately(t *testing.T) {
	h := laterHarness(t, -10, -12)
	startCycle(h, 4)
	failCore(h, 0)
	h.decide(h.next())
	failCore(h, 1)
	d, _ := nextDecision(h)
	if !isHold(d) || d.Core != 1 {
		t.Fatalf("a failure on another core is its own first failure: %+v", d)
	}
	h.decide(h.next())
	if got := len(h.s.later.strikes); got != 2 {
		t.Fatalf("%d strikes, want one per core", got)
	}
	if bios := h.s.BIOSProfile(); !slices.Equal(bios.Unconfirmed, []int{0, 1}) {
		t.Fatalf("BIOS profile %+v", bios)
	}
	assertLaterReplay(h)
}

func TestLaterWindowPairsWithinKCyclesAndExpiresAtK(t *testing.T) {
	for _, tt := range []struct {
		name string
		gap  int
		pair bool
	}{{"same cycle", 0, true}, {"last cycle of the window", laterWindow - 1, true}, {"one past the window", laterWindow, false}, {"long after", laterWindow + 4, false}} {
		t.Run(tt.name, func(t *testing.T) {
			h := laterHarness(t, -10, -12)
			startCycle(h, 4)
			first := failCore(h, 0)
			h.decide(h.next())
			if tt.gap > 0 {
				endCycle(h, 4)
				passCycles(h, 5, 4+tt.gap-1)
				startCycle(h, 4+tt.gap)
			}
			failCore(h, 0)
			d, a := nextDecision(h)
			if tt.pair {
				if isHold(d) || d.ToOffset != -9 || a.Cause[len(a.Cause)-1] != first.Seq {
					t.Fatalf("second failure within the window must step back citing the first: %+v %v", d, a.Cause)
				}
				return
			}
			if !isHold(d) {
				t.Fatalf("a failure past the window is a first failure again: %+v", d)
			}
			h.decide(a)
			if len(h.s.later.strikes) != 1 || h.s.later.strikes[0].failure == first.Seq {
				t.Fatalf("the expired strike must be replaced: %+v", h.s.later.strikes)
			}
			assertLaterReplay(h)
		})
	}
}

func TestLaterStrikeIsInvalidWhenItsCoreMovedForAnotherReason(t *testing.T) {
	h := laterHarness(t, -10, -12)
	startCycle(h, 4)
	failCore(h, 0)
	h.decide(h.next())
	h.add(&journal.TunerDecision{Core: 0, Phase: journal.PhaseHunt, Decision: journal.Backoff, FromOffset: -10, ToOffset: -9, Pass: new(-10), FailurePoint: new(-10), Reason: "test"})
	settle(h)
	endCycle(h, 4)
	if len(h.s.later.strikes) != 0 {
		t.Fatalf("a strike on a moved core stayed: %+v", h.s.later.strikes)
	}
	startCycle(h, 5)
	failCore(h, 0)
	if d, _ := nextDecision(h); !isHold(d) || d.FromOffset != -9 {
		t.Fatalf("a failure after another move is a first failure: %+v", d)
	}
	assertLaterReplay(h)
}

func TestLaterGateReopensOnlyAfterTheSteppedBackProfilePasses(t *testing.T) {
	h := laterHarness(t, -10, -12)
	startCycle(h, 4)
	failCore(h, 0)
	h.decide(h.next())
	failCore(h, 0)
	h.decide(h.next())
	settle(h)
	failCore(h, 1)
	d, _ := nextDecision(h)
	if isHold(d) || d.Core != 1 || d.ToOffset != -11 {
		t.Fatalf("a failure on a profile that has not passed a cycle steps back at once: %+v", d)
	}
	h.decide(h.next())
	settle(h)
	endCycle(h, 4)
	startCycle(h, 5)
	failCore(h, 0)
	if d, _ := nextDecision(h); !isHold(d) {
		t.Fatalf("after the cycle passed a first failure holds again: %+v", d)
	}
	assertLaterReplay(h)
}

func TestLaterStaleFailureAtADeeperOffsetIsNotAHold(t *testing.T) {
	h := laterHarness(t, -10, -12)
	startCycle(h, 4)
	h.add(&journal.Failure{Attribution: journal.Attributed, Core: new(0), Offset: new(-12), Condition: machine.Together, Regime: machine.R1, Profile: []int{-12, -12}, Signal: machine.ComputationError})
	d, a := nextDecision(h)
	if isHold(d) || d.FromOffset != d.ToOffset || !strings.Contains(d.Reason, "already shallower") {
		t.Fatalf("answer to a stale failure %+v", d)
	}
	h.decide(a)
	if len(h.s.later.strikes) != 0 || len(h.s.later.held) != 0 {
		t.Fatalf("an already-shallower backoff created a strike: %+v %v", h.s.later.strikes, h.s.later.held)
	}
}

func TestLaterDrainEmitsTheSameHoldAndPair(t *testing.T) {
	h := laterHarness(t, -10, -12)
	startCycle(h, 4)
	failCore(h, 0)
	drained, ok := h.s.Drain()
	if !ok || cmp.Diff(h.s.Next(), drained) != "" {
		t.Fatalf("drain %+v (%t), want next %+v", drained, ok, h.s.Next())
	}
	if d := drained.Payload.(*journal.TunerDecision); !isHold(d) {
		t.Fatalf("drain did not hold: %+v", d)
	}
	h.decide(drained)
	failCore(h, 0)
	drained, ok = h.s.Drain()
	if !ok || cmp.Diff(h.s.Next(), drained) != "" || len(drained.Cause) != 2 {
		t.Fatalf("drain %+v (%t), want the pair %+v", drained, ok, h.s.Next())
	}
}

func idleFailure(h *harness) journal.Event {
	h.t.Helper()
	return h.add(&journal.Failure{Attribution: journal.Unattributed, Condition: machine.Together, Profile: h.s.Profile(), Signal: machine.Crash})
}

func TestLaterUnattributedFailureIsHeldBeforeItsHunt(t *testing.T) {
	h := laterHarness(t, -10, -12)
	startCycle(h, 4)
	first := idleFailure(h)
	a := h.next()
	skip, ok := a.Payload.(*journal.HuntSkipped)
	if !ok || skip.Failure != first.Seq || !strings.HasPrefix(skip.Reason, "held") || len(h.s.later.held) != 0 {
		t.Fatalf("a first idle failure is not hunted: %+v", a)
	}
	if missing := missingTokens(skip.Reason, "cores [00, 01]", fmt.Sprintf("K = %d", laterWindow)); len(missing) > 0 {
		t.Fatalf("hold reason %q lacks %q", skip.Reason, missing)
	}
	h.decide(a)
	if len(h.s.queue) != 0 || len(h.s.later.strikes) != 1 || !h.s.later.held[first.Seq] {
		t.Fatalf("hold left queue %v strikes %+v", h.s.queue, h.s.later.strikes)
	}
	if bios := h.s.BIOSProfile(); !slices.Equal(bios.Unconfirmed, []int{0, 1}) {
		t.Fatalf("BIOS profile %+v", bios)
	}
	assertLaterReplay(h)

	second := idleFailure(h)
	start := driveToHuntStart(h)
	p := start.Payload.(*journal.HuntStart)
	if p.Failure != second.Seq || cmp.Diff([]int{second.Seq, first.Seq}, start.Cause) != "" {
		t.Fatalf("paired hunt %+v cause %v", p, start.Cause)
	}
	if missing := missingTokens(p.Reason, fmt.Sprintf("#%d", first.Seq), fmt.Sprintf("#%d", second.Seq)); len(missing) > 0 {
		t.Fatalf("hunt reason %q lacks %q", p.Reason, missing)
	}
	started := h.decide(start)
	if h.s.hunt.paired != first.Seq || len(h.s.later.strikes) != 0 {
		t.Fatalf("hunt paired %d strikes %+v", h.s.hunt.paired, h.s.later.strikes)
	}
	h.add(&journal.HuntEnd{Hunt: 1, Result: "culprit", Cores: []int{0}, Groups: 1}, started.Seq)
	d, _ := nextDecision(h)
	if d.Phase != journal.PhaseHunt || !strings.Contains(d.Reason, fmt.Sprintf("after failures #%d and #%d", first.Seq, second.Seq)) {
		t.Fatalf("hunt backoff %+v", d)
	}
	assertLaterReplay(h)
}

func TestLaterHeldUnattributedFailurePairsWithAnAttributedOneOnALoadedCore(t *testing.T) {
	h := laterHarness(t, -10, -12)
	startCycle(h, 4)
	first := idleFailure(h)
	h.decide(h.next())
	second := failCore(h, 1)
	d, a := nextDecision(h)
	if isHold(d) || d.Core != 1 || a.Cause[len(a.Cause)-1] != first.Seq || a.Cause[0] != second.Seq || !strings.Contains(d.Reason, fmt.Sprintf("#%d", first.Seq)) {
		t.Fatalf("cross-type pair %+v %v", d, a.Cause)
	}
	h.decide(a)
	if len(h.s.later.strikes) != 0 {
		t.Fatalf("strikes %+v", h.s.later.strikes)
	}
	assertLaterReplay(h)
}

func TestLaterMultiCoreR7FailuresHoldWithoutCountingTowardEscalation(t *testing.T) {
	h := r7Harness(t)
	passCycles(h, 1, 3)
	startCycle(h, 4)
	_, first := failLiveR7(h, journal.TrialEnd{Core: new(0), DurationS: 41})
	d, a := nextDecision(h)
	if !isHold(d) || d.Core != 0 || d.FromOffset != -30 || a.Cause[0] != first.Seq {
		t.Fatalf("R7 hold %+v %v", d, a.Cause)
	}
	h.decide(a)
	if len(h.s.loads) != 0 || len(h.s.obligations) != 0 {
		t.Fatalf("a hold counted toward escalation or queued a rerun: %v %v", h.s.loads, h.s.obligations)
	}
	if h.s.r7Handled[first.Seq][0] != true {
		t.Fatalf("a held R7 failure stayed pending: %v", h.s.r7Handled)
	}
	_, second := failLiveR7(h, journal.TrialEnd{Core: new(0), DurationS: 41})
	d, a = nextDecision(h)
	if isHold(d) || d.ToOffset <= d.FromOffset || a.Cause[0] != second.Seq || a.Cause[len(a.Cause)-1] != first.Seq {
		t.Fatalf("R7 pair %+v %v", d, a.Cause)
	}
	if missing := missingTokens(d.Reason, fmt.Sprintf("#%d", first.Seq), fmt.Sprintf("#%d", second.Seq)); len(missing) > 0 {
		t.Fatalf("R7 pair reason %q lacks %q", d.Reason, missing)
	}
	h.decide(a)
	assertLaterReplay(h)
}

func TestBIOSProfileTracksTheConfirmedProfile(t *testing.T) {
	h := hasRoomHarness(t, -10, -12)
	if bios := h.s.BIOSProfile(); bios.Confirmed != 0 || len(bios.Unconfirmed) != 0 {
		t.Fatalf("before phase 1 ends: %+v", bios)
	}
	phase1 := passCycles(h, 1, 1)
	bios := h.s.BIOSProfile()
	if bios.Confirmed != phase1.Seq || !slices.Equal(bios.Offsets, []int{-10, -12}) || len(bios.Unconfirmed) != 0 {
		t.Fatalf("at phase 1's end: %+v", bios)
	}
	h.add(&journal.TunerDecision{Core: 0, Phase: journal.PhaseDeepening, Decision: journal.Deepen, FromOffset: -10, ToOffset: -11, Pass: new(-11), Reason: "round 1"})
	if bios := h.s.BIOSProfile(); !slices.Equal(bios.Offsets, []int{-10, -12}) || len(bios.Unconfirmed) != 0 {
		t.Fatalf("a deepened core keeps showing its confirmed offset: %+v", bios)
	}
	h.add(&journal.TunerDecision{Core: 0, Phase: journal.PhaseChecking, Decision: journal.Backoff, FromOffset: -11, ToOffset: -9, Pass: new(-10), FailurePoint: new(-11), Reason: "confirmation"})
	if bios := h.s.BIOSProfile(); !slices.Equal(bios.Offsets, []int{-9, -12}) || !slices.Equal(bios.Unconfirmed, []int{0}) {
		t.Fatalf("a step-back shallower than the confirmed offset replaces the profile unconfirmed: %+v", bios)
	}
	h.add(&journal.CommandReset{Core: new(1)})
	h.add(&journal.CorePhase{Core: 1, From: journal.PhaseAtLimit, To: journal.PhaseSearch, Offset: 0})
	if bios := h.s.BIOSProfile(); bios.Confirmed != 0 || !slices.Equal(bios.Offsets, []int{-9, 0}) {
		t.Fatalf("a reset returns to phase 1 with its core searching at 0: %+v", bios)
	}
}

func TestBIOSProfileMarksAStepBackSinceItsDecision(t *testing.T) {
	h := laterHarness(t, -10, -12)
	h.add(&journal.TunerDecision{Core: 1, Phase: journal.PhaseChecking, Decision: journal.Backoff, FromOffset: -12, ToOffset: -11, Pass: new(-12), FailurePoint: new(-12), Reason: "test"})
	bios := h.s.BIOSProfile()
	if !slices.Equal(bios.Offsets, []int{-10, -11}) || !slices.Equal(bios.Unconfirmed, []int{1}) || bios.Since == 0 {
		t.Fatalf("%+v", bios)
	}
}

func TestLaterOffsetsNeverDeepenAfterConclusion(t *testing.T) {
	h := laterHarness(t, -10, -12)
	startCycle(h, 4)
	before := h.s.offsets()
	for range 6 {
		failCore(h, 0)
		failCore(h, 1)
		settle(h)
		for i, v := range h.s.offsets() {
			if v < before[i] {
				t.Fatalf("core %d deepened from %d to %d", i, before[i], v)
			}
		}
		before = h.s.offsets()
		endCycle(h, h.s.checking.cycle)
		startCycle(h, h.s.checking.cycle+1)
	}
	assertLaterReplay(h)
}

// assertShown pins the BIOS profile of the live, incrementally folded state and of a fresh fold of the same journal.
func assertShown(t *testing.T, h *harness, want BIOSProfile) {
	t.Helper()
	if diff := cmp.Diff(want, h.s.BIOSProfile()); diff != "" {
		t.Fatalf("live BIOS profile (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff(want, replayState(h.events).BIOSProfile()); diff != "" {
		t.Fatalf("replayed BIOS profile (-want +got):\n%s", diff)
	}
}

// holdCase holds the first failure of a later-failure test and names the profile it shows.
type holdCase struct {
	name        string
	offsets     []int
	hold        func(h *harness) journal.Event
	shown       []int
	unconfirmed []int
}

func holdCases() []holdCase {
	return []holdCase{
		{"attributed", []int{-10, -12}, func(h *harness) journal.Event {
			failCore(h, 0)
			return h.decide(h.next())
		}, []int{-9, -12}, []int{0}},
		{"unattributed steps back its nonzero candidates and leaves cores at 0", []int{-10, 0, -12}, func(h *harness) journal.Event {
			idleFailure(h)
			a := h.next()
			if _, ok := a.Payload.(*journal.HuntSkipped); !ok {
				h.t.Fatalf("a first idle failure is not held: %+v", a)
			}
			return h.decide(a)
		}, []int{-9, 0, -11}, []int{0, 2}},
	}
}

func TestBIOSProfileShowsAHeldFailureSteppedBackAtOnce(t *testing.T) {
	for _, tt := range holdCases() {
		t.Run(tt.name, func(t *testing.T) {
			h := laterHarness(t, tt.offsets...)
			confirmed := h.s.BIOSProfile().Confirmed
			startCycle(h, 4)
			hold := tt.hold(h)
			assertShown(t, h, BIOSProfile{Offsets: tt.shown, Confirmed: confirmed, Unconfirmed: tt.unconfirmed, Since: hold.Seq})
			if diff := cmp.Diff(tt.offsets, h.s.offsets()); diff != "" {
				t.Fatalf("a hold moved a core (-want +got):\n%s", diff)
			}
			assertLaterReplay(h)
		})
	}
}

func TestBIOSProfileShowsAMultiCountR7HoldAtTheVoltageTarget(t *testing.T) {
	h := r7Harness(t)
	for range config.Default().Evidence.Trials() {
		r7Fact(h, true, []int{0, 1}, h.s.Profile(), map[int]float64{0: 1.1539, 1: 1.08}, []int{0}, nil, nil, nil)
	}
	passCycles(h, 1, 3)
	confirmed := h.s.BIOSProfile().Confirmed
	startCycle(h, 4)
	failLiveR7(h, journal.TrialEnd{Core: new(0), DurationS: 41, TopRequesters: []int{0}, VoltageRequestsV: map[int]float64{0: 1.09447, 1: 1.08}})
	d, a := nextDecision(h)
	if !isHold(d) || d.Core != 0 {
		t.Fatalf("R7 hold %+v", d)
	}
	hold := h.decide(a)
	assertShown(t, h, BIOSProfile{Offsets: []int{-13, -30, -30, -30}, Confirmed: confirmed, Unconfirmed: []int{0}, Since: hold.Seq})
	assertLaterReplay(h)
}

func TestBIOSProfileFollowsTheActualStepBackAtASecondFailure(t *testing.T) {
	for _, tt := range []struct {
		name              string
		hold              func(h *harness)
		second            func(h *harness) journal.Event
		core              int
		before            []int
		beforeUnconfirmed []int
		after             []int
	}{
		{"same core", func(h *harness) { failCore(h, 0); h.decide(h.next()) }, func(h *harness) journal.Event { return failCore(h, 0) }, 0, []int{-9, -12}, []int{0}, []int{-9, -12}},
		{"attributed failure pairs with a held unattributed one", func(h *harness) { idleFailure(h); h.decide(h.next()) }, func(h *harness) journal.Event { return failCore(h, 1) }, 1, []int{-9, -11}, []int{0, 1}, []int{-10, -11}},
	} {
		t.Run(tt.name, func(t *testing.T) {
			h := laterHarness(t, -10, -12)
			confirmed := h.s.BIOSProfile().Confirmed
			startCycle(h, 4)
			tt.hold(h)
			hold := h.events[len(h.events)-1]
			tt.second(h)
			assertShown(t, h, BIOSProfile{Offsets: tt.before, Confirmed: confirmed, Unconfirmed: tt.beforeUnconfirmed, Since: hold.Seq})
			d, a := nextDecision(h)
			if isHold(d) || d.Core != tt.core || d.ToOffset <= d.FromOffset {
				t.Fatalf("second failure within K must step back: %+v", d)
			}
			step := h.decide(a)
			assertShown(t, h, BIOSProfile{Offsets: tt.after, Confirmed: confirmed, Unconfirmed: []int{tt.core}, Since: step.Seq})
			assertLaterReplay(h)
		})
	}
}

func TestBIOSProfileReturnsToTheHeldOffsetWhenTheHoldExpires(t *testing.T) {
	for _, tt := range holdCases() {
		t.Run(tt.name, func(t *testing.T) {
			h := laterHarness(t, tt.offsets...)
			startCycle(h, 4)
			hold := tt.hold(h)
			end := endCycle(h, 4)
			last := passCycles(h, 5, 4+laterWindow-1)
			if last.Seq <= end.Seq {
				t.Fatal("no cycle passed")
			}
			assertShown(t, h, BIOSProfile{Offsets: tt.shown, Confirmed: last.Seq, Unconfirmed: tt.unconfirmed, Since: hold.Seq})
			startCycle(h, 4+laterWindow)
			assertShown(t, h, BIOSProfile{Offsets: tt.offsets, Confirmed: last.Seq})
			assertLaterReplay(h)
		})
	}
}

// A located hunt's outcome is a hunt-phase answer: it is never held, even when the gate is open.
func TestLaterLocatedGroupFailureNamingALoadedCoreIsNotHeld(t *testing.T) {
	h := r7Harness(t)
	end := journal.TrialEnd{DurationS: 41, TopRequesters: []int{0}}
	backOffEscalatingR7(h, end)
	passCycles(h, 1, 3)
	startCycle(h, 4)
	if !h.s.laterGate() {
		t.Fatal("later-failure gate closed")
	}
	failLiveR7(h, end)
	a := runLocated(h, func(p []int) *int {
		if p[2] == 0 {
			return nil
		}
		return new(0)
	})
	if he, ok := a.Payload.(*journal.HuntEnd); !ok || he.Result != "loaded" || !slices.Equal(he.Cores, []int{0, 1}) {
		t.Fatalf("hunt end %+v", a)
	}
	h.decide(a)
	d, a := nextDecision(h)
	if isHold(d) || d.Core != 0 || d.ToOffset != -29 || d.FailurePoint == nil || *d.FailurePoint != -30 {
		t.Fatalf("the group failure's ordinary backoff expected: %+v", d)
	}
	h.decide(a)
	if _, pending := h.s.Drain(); pending {
		t.Fatal("the failure moved twice")
	}
	if len(h.s.later.strikes) != 0 {
		t.Fatalf("a located outcome created a strike: %+v", h.s.later.strikes)
	}
	assertLaterReplay(h)
}

func TestLaterPairedHuntEndingDirectNamesBothFailuresInTheBackoff(t *testing.T) {
	h := laterHarness(t, -10, -12)
	startCycle(h, 4)
	first := idleFailure(h)
	h.decide(h.next())
	second := idleFailure(h)
	start := driveToHuntStart(h)
	started := h.decide(start)
	failure := h.add(&journal.Failure{Attribution: journal.Attributed, Core: new(0), Offset: new(-10), Condition: machine.Parked, Regime: machine.R1, Profile: []int{-10, 0}, Signal: machine.ComputationError})
	a := h.next()
	end, ok := a.Payload.(*journal.HuntEnd)
	if !ok || end.Result != "direct" {
		t.Fatalf("hunt end %+v", a)
	}
	h.decide(a)
	d, a := nextDecision(h)
	want := fmt.Sprintf("after failures #%d and #%d", first.Seq, second.Seq)
	if d.Phase != journal.PhaseHunt || d.Core != 0 || !strings.Contains(d.Reason, want) {
		t.Fatalf("direct backoff %+v lacks %q (failure #%d, hunt #%d)", d, want, failure.Seq, started.Seq)
	}
	h.decide(a)
	assertLaterReplay(h)
}

func TestBIOSProfileKeepsAPairedHuntsHoldShownUntilItsCommitment(t *testing.T) {
	h := laterHarness(t, -10, -12)
	confirmed := h.s.BIOSProfile().Confirmed
	startCycle(h, 4)
	idleFailure(h)
	hold := h.decide(h.next())
	idleFailure(h)
	shown := BIOSProfile{Offsets: []int{-9, -11}, Confirmed: confirmed, Unconfirmed: []int{0, 1}, Since: hold.Seq}
	assertShown(t, h, shown)
	started := h.decide(driveToHuntStart(h))
	assertShown(t, h, shown)
	if len(h.s.later.strikes) != 0 || h.s.hunt == nil || h.s.hunt.paired == 0 {
		t.Fatalf("the paired hunt did not consume the hold: strikes %+v", h.s.later.strikes)
	}
	if a := h.next(); a.Kind == Decide {
		if _, ok := a.Payload.(*journal.HuntGroup); ok {
			h.decide(a)
			assertShown(t, h, shown)
		}
	}
	end := h.add(&journal.HuntEnd{Hunt: 1, Result: "culprit", Cores: []int{0}, Groups: 1}, started.Seq)
	assertShown(t, h, shown)
	d, a := nextDecision(h)
	if d.Phase != journal.PhaseHunt || d.Core != 0 || a.Cause[0] != end.Seq {
		t.Fatalf("hunt backoff %+v", d)
	}
	assertShown(t, h, shown)
	commit := h.decide(a)
	assertShown(t, h, BIOSProfile{Offsets: []int{-9, -12}, Confirmed: confirmed, Unconfirmed: []int{0}, Since: commit.Seq})
	assertLaterReplay(h)
}

// failLoadingOnly fails a together trial that loads only core id, with no attribution to any core.
func failLoadingOnly(h *harness, id int) journal.Event {
	h.t.Helper()
	tr := Trial{Regime: machine.R1, Workload: machine.Workloads(machine.R1)[0].ID, Condition: machine.Together, Phase: journal.PhaseChecking, DurationS: 120, Core: id, Offset: h.s.core(id).offset, Cycle: h.s.checking.cycle}
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, DurationS: 41})
	a := h.next()
	if f, ok := a.Payload.(*journal.Failure); !ok || f.Attribution != journal.Unattributed {
		h.t.Fatalf("want an unattributed failure, got %+v", a)
	}
	return h.decide(a)
}

// A hold's target on a core that really steps back and is then confirmed stops showing; the other core's target stays.
func TestBIOSProfileDropsAHoldTargetOnceItsCoreIsConfirmedAtIt(t *testing.T) {
	h := laterHarness(t, -10, -12)
	startCycle(h, 4)
	first := failLoadingOnly(h, 0)
	a := h.next()
	skip, ok := a.Payload.(*journal.HuntSkipped)
	if !ok || skip.Failure != first.Seq {
		t.Fatalf("a first unattributed failure is held: %+v", a)
	}
	hold := h.decide(a)
	if len(h.s.later.strikes) != 1 || !slices.Equal(h.s.later.strikes[0].cores, []int{0}) {
		t.Fatalf("strikes %+v, want one over core 0", h.s.later.strikes)
	}
	assertShown(t, h, BIOSProfile{Offsets: []int{-9, -11}, Confirmed: h.s.BIOSProfile().Confirmed, Unconfirmed: []int{0, 1}, Since: hold.Seq})

	failCore(h, 1)
	if d, _ := nextDecision(h); !isHold(d) || d.Core != 1 {
		t.Fatalf("core 1's own first failure is held: %+v", d)
	}
	h.decide(h.next())
	failCore(h, 1)
	d, a := nextDecision(h)
	if isHold(d) || d.Core != 1 || d.ToOffset != -11 {
		t.Fatalf("core 1's second failure steps it back: %+v", d)
	}
	h.decide(a)
	settle(h)
	end := endCycle(h, 4)
	assertShown(t, h, BIOSProfile{Offsets: []int{-9, -11}, Confirmed: end.Seq, Unconfirmed: []int{0}, Since: hold.Seq})
	if len(h.s.later.strikes) != 1 || h.s.later.strikes[0].failure != first.Seq {
		t.Fatalf("the original strike was not preserved: %+v", h.s.later.strikes)
	}
	assertLaterReplay(h)
}

// Between a paired hunt's combination and its backoff the shown profile must not reach the combination the hunt learned.
func TestBIOSProfileProjectsAPairedHuntsPendingBackoffPastItsCombination(t *testing.T) {
	h := laterHarness(t, -10, -12)
	confirmed := h.s.BIOSProfile().Confirmed
	startCycle(h, 4)
	idleFailure(h)
	hold := h.decide(h.next())
	idleFailure(h)
	h.decide(driveToHuntStart(h))
	fails := func(p []int) bool { return p[0] <= -9 && p[1] <= -11 }
	for range 200 {
		a := h.next()
		if _, ok := a.Payload.(*journal.HuntGroup); ok {
			h.decide(a)
			continue
		}
		if a.Kind == RunTrial {
			runGroup(h, a, fails(a.Trial.Profile))
			continue
		}
		if _, ok := a.Payload.(*journal.HuntEnd); ok {
			h.decide(a)
			break
		}
		t.Fatalf("unexpected action %+v", a)
	}
	combination := h.next()
	c, ok := combination.Payload.(*journal.Combination)
	if !ok || cmp.Diff([]journal.CombinationMember{{Core: 0, Offset: -9}, {Core: 1, Offset: -11}}, c.Members) != "" {
		t.Fatalf("combination %+v", combination)
	}
	h.decide(combination)
	back, a := nextDecision(h)
	if back.Phase != journal.PhaseHunt || back.Decision != journal.Backoff || back.ToOffset <= back.FromOffset {
		t.Fatalf("commitment %+v", back)
	}
	pending := h.s.BIOSProfile()
	if _, reached := h.s.Reaches(pending.Offsets); reached {
		t.Fatalf("the shown profile %v reaches the combination the hunt learned", pending.Offsets)
	}
	if pending.Offsets[h.s.index(back.Core)] != back.ToOffset || pending.Since != hold.Seq || !slices.Equal(pending.Unconfirmed, []int{0, 1}) {
		t.Fatalf("pending commitment %+v, want core %02d at %d unconfirmed since the hold #%d", pending, back.Core, back.ToOffset, hold.Seq)
	}
	assertShown(t, h, pending)
	commit := h.decide(a)
	after := h.s.BIOSProfile()
	if after.Offsets[h.s.index(back.Core)] != back.ToOffset || h.s.hunt != nil {
		t.Fatalf("after the commitment %+v", after)
	}
	if diff := cmp.Diff(BIOSProfile{Offsets: after.Offsets, Confirmed: confirmed, Unconfirmed: []int{back.Core}, Since: commit.Seq}, after); diff != "" {
		t.Fatalf("after the commitment (-want +got):\n%s", diff)
	}
	assertShown(t, h, after)
	assertLaterReplay(h)
}

// A direct end names the core the probe observed failing: the shown profile moves it at once, before the tuning move.
func TestBIOSProfileProjectsAPairedHuntsDirectBackoffBeforeItIsRecorded(t *testing.T) {
	h := laterHarness(t, -10, -12)
	confirmed := h.s.BIOSProfile().Confirmed
	startCycle(h, 4)
	idleFailure(h)
	hold := h.decide(h.next())
	idleFailure(h)
	h.decide(driveToHuntStart(h))
	h.add(&journal.Failure{Attribution: journal.Attributed, Core: new(1), Offset: new(-11), Condition: machine.Parked, Regime: machine.R1, Profile: []int{-9, -11}, Signal: machine.ComputationError})
	a := h.next()
	if end, ok := a.Payload.(*journal.HuntEnd); !ok || end.Result != "direct" {
		t.Fatalf("hunt end %+v", a)
	}
	h.decide(a)
	if diff := cmp.Diff([]int{-10, -12}, h.s.offsets()); diff != "" {
		t.Fatalf("the tuning profile moved before the backoff (-want +got):\n%s", diff)
	}
	pending := BIOSProfile{Offsets: []int{-9, -10}, Confirmed: confirmed, Unconfirmed: []int{0, 1}, Since: hold.Seq}
	assertShown(t, h, pending)
	d, a := nextDecision(h)
	if d.Phase != journal.PhaseHunt || d.Core != 1 || d.ToOffset != -10 {
		t.Fatalf("direct backoff %+v", d)
	}
	commit := h.decide(a)
	assertShown(t, h, BIOSProfile{Offsets: []int{-10, -10}, Confirmed: confirmed, Unconfirmed: []int{1}, Since: commit.Seq})
	if h.s.hunt != nil || len(h.s.later.strikes) != 0 {
		t.Fatalf("hold stayed after the commitment: hunt %v strikes %+v", h.s.hunt, h.s.later.strikes)
	}
	assertLaterReplay(h)
}
