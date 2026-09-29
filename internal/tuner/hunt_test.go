package tuner

import (
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func huntHarness(t *testing.T, cores int, duration int) *harness {
	starts := make([]coreStart, cores)
	for i := range starts {
		starts[i] = coreStart{phase: journal.PhaseDone, offset: -30, fail: new(-31)}
	}
	h := newHarness(t, starts...)
	h.decide(h.next())
	ids := h.s.ids()
	tr := Trial{Regime: machine.R7, Cores: ids, Workload: machine.Workloads(machine.R7)[0].ID, Condition: machine.Resident, Phase: journal.PhaseGuard, DurationS: duration, Profile: h.s.offsets()}
	h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 5})
	a := h.next()
	if f, ok := a.Payload.(*journal.Failure); !ok || f.Attribution != journal.Unattributed {
		t.Fatalf("source attribution %+v", a)
	}
	h.decide(a)
	for range 50 {
		a = h.next()
		if _, ok := a.Payload.(*journal.HuntStart); ok {
			h.decide(a)
			return h
		}
		if a.Kind != Decide {
			t.Fatalf("expected hunt start, got %+v", a)
		}
		h.decide(a)
	}
	t.Fatal("hunt not started")
	return nil
}

func runMask(h *harness, a Action, fail bool) {
	h.t.Helper()
	if a.Kind != RunTrial || a.Trial.Condition != machine.Masked {
		h.t.Fatalf("mask trial %+v", a)
	}
	end := passed
	if fail {
		end = journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 5}
	}
	h.trial(a, end)
	if fail {
		attribution := h.next()
		if f, ok := attribution.Payload.(*journal.Failure); !ok || f.Attribution != journal.Unattributed {
			h.t.Fatalf("unexpected mask attribution %+v", attribution)
		}
		h.decide(attribution)
	}
}

func TestAllZeroHuntAnchorOmitsZeroCause(t *testing.T) {
	h := huntHarness(t, 4, 120)
	for _, e := range h.events {
		p, ok := e.Data.(*journal.HuntStart)
		if !ok {
			continue
		}
		if p.AnchorSeq != 0 {
			t.Fatalf("anchor seq %d, want all-zero anchor", p.AnchorSeq)
		}
		if diff := cmp.Diff([]int{p.Failure}, e.Cause); diff != "" {
			t.Fatalf("hunt cause (-want +got):\n%s", diff)
		}
		return
	}
	t.Fatal("no hunt.start")
}
func TestHuntPairAndCommitmentResume(t *testing.T) {
	h := huntHarness(t, 4, 120)
	for range 200 {
		replay := New()
		for _, e := range h.events {
			replay.Fold(e)
		}
		if diff := cmp.Diff(h.s.Next(), replay.Next()); diff != "" {
			t.Fatalf("hunt prefix replay (-live +replay):\n%s", diff)
		}
		a := h.next()
		if mask, ok := a.Payload.(*journal.HuntMask); ok {
			h.decide(a)
			if mask.Mask == 1 && !slices.Equal(mask.Cores, []int{2, 3}) {
				t.Fatalf("recent mark did not move its part first: %v", mask.Cores)
			}
			continue
		}
		if a.Kind == RunTrial {
			fail := slices.Equal(a.Trial.Profile, []int{-30, -30, 0, 0})
			runMask(h, a, fail)
			continue
		}
		if end, ok := a.Payload.(*journal.HuntEnd); ok {
			if end.Result != "joint" || cmp.Diff([]int{0, 1}, end.Cores) != "" {
				t.Fatalf("hunt result %+v", end)
			}
			h.decide(a)
			break
		}
		t.Fatalf("unexpected action %+v", a)
	}
	mark := h.next()
	p, ok := mark.Payload.(*journal.MarkJoint)
	if !ok || cmp.Diff([]journal.JointMember{{Core: 0, Offset: -30}, {Core: 1, Offset: -30}}, p.Members) != "" {
		t.Fatalf("mark %+v", mark)
	}
	h.decide(mark)
	replay := New()
	for _, e := range h.events {
		replay.Fold(e)
	}
	if diff := cmp.Diff(h.s.Next(), replay.Next()); diff != "" {
		t.Fatalf("commitment resume (-live +replay):\n%s", diff)
	}
	back := h.next()
	d, ok := back.Payload.(*journal.TunerDecision)
	if !ok || d.Decision != journal.Backoff || d.Phase != journal.PhaseHunt {
		t.Fatalf("commitment %+v", back)
	}
	h.decide(back)
	if h.s.hunt != nil {
		t.Fatal("hunt reopened after commitment")
	}
	if _, reached := h.s.Reaches(h.s.offsets()); reached {
		t.Fatalf("commitment reached mark: %v", h.s.offsets())
	}
}

func TestHuntFallbackAndFullCheck(t *testing.T) {
	for _, tc := range []struct {
		name      string
		duration  int
		fullFails bool
		result    string
	}{{"fallback", 120, false, "fallback"}, {"full failure", 600, true, "joint"}} {
		t.Run(tc.name, func(t *testing.T) {
			h := huntHarness(t, 2, tc.duration)
			full := 0
			for range 100 {
				a := h.next()
				if mask, ok := a.Payload.(*journal.HuntMask); ok {
					if mask.Stage == "full" {
						full++
					}
					h.decide(a)
					continue
				}
				if a.Kind == RunTrial {
					fail := tc.fullFails && slices.Equal(a.Trial.Profile, []int{-30, -30})
					runMask(h, a, fail)
					continue
				}
				if end, ok := a.Payload.(*journal.HuntEnd); ok {
					if end.Result != tc.result {
						t.Fatalf("result %s, want %s", end.Result, tc.result)
					}
					if tc.fullFails && full != 1 {
						t.Fatalf("full mask recorded %d times", full)
					}
					return
				}
				t.Fatalf("unexpected action %+v", a)
			}
			t.Fatal("hunt did not end")
		})
	}
}

func TestHuntAnchorSelectionAndReset(t *testing.T) {
	h := huntHarness(t, 4, 120)
	start := h.s.hunt.start
	if start.AnchorSeq != 0 || !slices.Equal(start.Anchor, []int{0, 0, 0, 0}) {
		t.Fatalf("all-zero anchor %+v", start)
	}
	h.add(&journal.CommandReset{Core: new(0)})
	a := h.next()
	end, ok := a.Payload.(*journal.HuntEnd)
	if !ok || end.Result != "cancelled" {
		t.Fatalf("reset did not cancel hunt: %+v", a)
	}
	h.decide(a)
	if h.s.hunt != nil || len(h.s.queue) != 1 || h.s.queue[0].seq != start.Failure || h.s.queue[0].class.regime != machine.R7 {
		t.Fatalf("source not requeued with class after cancellation: %+v", h.s.queue)
	}
}

func TestHuntEscalationAndInferredMask(t *testing.T) {
	h := huntHarness(t, 2, 600)
	first := h.next()
	mask, ok := first.Payload.(*journal.HuntMask)
	if !ok {
		t.Fatalf("first mask %+v", first)
	}
	profile := slices.Clone(mask.Profile)
	tr := Trial{Regime: machine.R7, Cores: []int{0, 1}, Workload: machine.Workloads(machine.R7)[0].ID, Condition: machine.Masked, Phase: journal.PhaseHunt, DurationS: 120, Profile: profile}
	for range h.s.n {
		h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
	}
	first = h.next()
	mask, ok = first.Payload.(*journal.HuntMask)
	if !ok || mask.Inferred != "pass" {
		t.Fatalf("inferred mask %+v", first)
	}
	h.decide(first)
	escalated := false
	for range 200 {
		a := h.next()
		if mask, ok := a.Payload.(*journal.HuntMask); ok {
			if mask.Escalated {
				escalated = true
				if mask.DurationS != 600 {
					t.Fatalf("escalated duration %d", mask.DurationS)
				}
			}
			h.decide(a)
			continue
		}
		if a.Kind == RunTrial {
			runMask(h, a, false)
			continue
		}
		if _, ok := a.Payload.(*journal.HuntEnd); ok {
			if !escalated {
				t.Fatal("full pass never escalated")
			}
			return
		}
		t.Fatalf("unexpected action %+v", a)
	}
	t.Fatal("escalated hunt did not finish")
}

func TestHuntSkipsMarkedMask(t *testing.T) {
	h := huntHarness(t, 4, 120)
	h.add(&journal.MarkJoint{Mark: 1, Hunt: 999, Members: []journal.JointMember{{Core: 2, Offset: -30}, {Core: 3, Offset: -30}}})
	a := h.next()
	m, ok := a.Payload.(*journal.HuntMask)
	if !ok || !m.Skipped {
		t.Fatalf("mask on newly marked profile %+v", a)
	}
	h.decide(a)
	if next := h.next(); next.Kind == RunTrial && next.Trial.Mask == m.Mask {
		t.Fatalf("ran skipped mask %+v", next)
	}
}

func TestHuntAnchorSkipsExactQualifiedProfile(t *testing.T) {
	starts := make([]coreStart, 2)
	for i := range starts {
		starts[i] = coreStart{phase: journal.PhaseDone, offset: -30, fail: new(-31)}
	}
	h := newHarness(t, starts...)
	h.add(&journal.ProfileChange{To: []int{-20, -20}})
	h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationStart, Steps: h.s.steps})
	old := h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationEnd, Clean: true, Qualifying: true})
	h.add(&journal.ProfileChange{From: []int{-20, -20}, To: []int{-30, -30}})
	h.add(&journal.GuardRotation{Rotation: 2, Event: journal.RotationStart, Steps: h.s.steps})
	h.add(&journal.GuardRotation{Rotation: 2, Event: journal.RotationEnd, Clean: true, Qualifying: true})
	failure := h.add(&journal.Failure{Attribution: journal.Unattributed, Signal: machine.Crash, Condition: machine.Resident, Regime: machine.R6, Profile: []int{-30, -30}})
	a := h.s.huntStartNext()
	start, ok := a.Payload.(*journal.HuntStart)
	if !ok || start.Failure != failure.Seq || start.AnchorSeq != old.Seq || cmp.Diff([]int{-20, -20}, start.Anchor) != "" {
		t.Fatalf("anchor %+v, want older #%d", a, old.Seq)
	}
}

func TestHuntLoadedIdleSplit(t *testing.T) {
	h := huntHarness(t, 4, 120)
	h.s.recent = nil
	h.s.hunt.start.Cores = []int{0, 2}
	want := [][]int{{0, 2}, {1, 3}}
	if got := h.s.split(h.s.hunt, []int{0, 1, 2, 3}, 2); cmp.Diff(want, got) != "" {
		t.Fatalf("first split (-want +got):\n%s", cmp.Diff(want, got))
	}
}

func TestHuntJointAlreadyBroken(t *testing.T) {
	h := residentHarness(t, -29, -30)
	start := &journal.HuntStart{Hunt: 1, Failure: 10, Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, DurationS: 120, Failing: []int{-30, -30}, Anchor: []int{0, 0}, Candidates: []int{0, 1}, Starts: 5, StartS: 120}
	h.add(start)
	h.add(&journal.HuntEnd{Hunt: 1, Result: "joint", Cores: []int{0, 1}, Masks: 2})
	a := h.next()
	mark, ok := a.Payload.(*journal.MarkJoint)
	if !ok || mark.Reason != "no backoff: core 00 is already shallower" {
		t.Fatalf("unneeded backoff %+v", a)
	}
	h.decide(a)
	if h.s.hunt != nil {
		t.Fatal("joint commitment did not close when resident profile already breaks it")
	}
}

func TestSixteenCoreCulpritAndPair(t *testing.T) {
	for _, tc := range []struct {
		name    string
		members []int
		result  string
	}{
		{"single core", []int{13}, "direct"},
		{"joint pair", []int{3, 11}, "joint"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := huntHarness(t, 16, 120)
			ended := false
			for range 2500 {
				a := h.next()
				if mask, ok := a.Payload.(*journal.HuntMask); ok {
					h.decide(a)
					if mask.Mask > 194 {
						t.Fatalf("hunt exceeded progress bound: %d masks", mask.Mask)
					}
					continue
				}
				if a.Kind == RunTrial {
					fail := true
					for _, member := range tc.members {
						fail = fail && a.Trial.Profile[member] == -30
					}
					outcome := passed
					if fail {
						outcome = journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 5}
					}
					h.trial(a, outcome)
					if fail {
						attribution := h.next()
						if _, ok := attribution.Payload.(*journal.Failure); !ok {
							t.Fatalf("mask failure did not produce attribution: %+v", attribution)
						}
						h.decide(attribution)
					}
					continue
				}
				if end, ok := a.Payload.(*journal.HuntEnd); ok {
					if end.Result != tc.result || cmp.Diff(tc.members, end.Cores) != "" {
						t.Fatalf("hunt result %+v, want %s %v", end, tc.result, tc.members)
					}
					h.decide(a)
					ended = true
					break
				}
				t.Fatalf("unexpected hunt action %+v", a)
			}
			if !ended {
				t.Fatal("sixteen-core hunt did not terminate")
			}
		})
	}
}
