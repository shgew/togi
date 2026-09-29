package tuner

import (
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
)

func qualifiedHarness(t *testing.T, offsets []int, marks [][]int) *harness {
	t.Helper()
	starts := make([]coreStart, len(offsets))
	for i, v := range offsets {
		starts[i] = coreStart{phase: journal.PhaseDone, offset: v}
	}
	h := newHarness(t, starts...)
	for i, pair := range marks {
		h.add(&journal.MarkJoint{Mark: i + 1, Hunt: i + 1, Members: []journal.JointMember{{Core: pair[0], Offset: -50}, {Core: pair[1], Offset: -50}}})
	}
	h.add(&journal.ProfileChange{To: slices.Clone(offsets)})
	h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationStart, Steps: h.s.steps})
	h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationEnd, Clean: true, Qualifying: true})
	return h
}

func nextRound(h *harness) *journal.RefineRound {
	h.t.Helper()
	for range 20 {
		a := h.next()
		if p, ok := a.Payload.(*journal.RefineRound); ok && p.Event == journal.RotationStart {
			h.decide(a)
			return p
		}
		if a.Kind != Decide {
			h.t.Fatalf("expected refine round: %+v", a)
		}
		h.decide(a)
	}
	h.t.Fatal("no refinement round")
	return nil
}

func TestRefineGlobalOptimumAndResume(t *testing.T) {
	marks := [][]int{{0, 1}, {1, 2}, {2, 3}, {3, 0}, {1, 3}}
	h := qualifiedHarness(t, []int{-49, -49, -49, -50}, marks)
	r := nextRound(h)
	if diff := cmp.Diff([]int{-50, -49, -50, -49}, r.Target); diff != "" {
		t.Fatalf("global optimum (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff(r.Target, r.Profile); diff != "" {
		t.Fatalf("proposed profile (-want +got):\n%s", diff)
	}
	decisions := []journal.Decision{journal.Yield, journal.Deepen, journal.Deepen}
	for _, want := range decisions {
		live := h.next()
		replayed := New()
		for _, e := range h.events {
			replayed.Fold(e)
		}
		if diff := cmp.Diff(live, replayed.Next()); diff != "" {
			t.Fatalf("round resume (-live +replayed):\n%s", diff)
		}
		d, ok := live.Payload.(*journal.TunerDecision)
		if !ok || d.Decision != want {
			t.Fatalf("move %+v, want %s", live, want)
		}
		h.decide(live)
	}
	for range 15 {
		a := h.next()
		if _, ok := a.Payload.(*journal.ProfileChange); ok {
			h.decide(a)
			break
		}
		if a.Kind != Decide {
			t.Fatalf("expected profile change: %+v", a)
		}
		h.decide(a)
	}
	if diff := cmp.Diff(r.Profile, h.s.Profile()); diff != "" {
		t.Fatalf("profile (-want +got):\n%s", diff)
	}
	for range 100 {
		replayed := New()
		for _, e := range h.events {
			replayed.Fold(e)
		}
		if diff := cmp.Diff(h.s.Next(), replayed.Next()); diff != "" {
			t.Fatalf("refine prefix replay (-live +replayed):\n%s", diff)
		}
		a := h.next()
		if a.Kind == RunTrial {
			if a.Trial.Round != r.Round {
				t.Fatalf("trial lost round: %+v", a.Trial)
			}
			h.trial(a, passed)
			continue
		}
		if end, ok := a.Payload.(*journal.RefineRound); ok && end.Event == journal.RotationEnd {
			if !end.Passed {
				t.Fatalf("failed round %+v", end)
			}
			h.decide(a)
			return
		}
		t.Fatalf("unexpected check action %+v", a)
	}
	t.Fatal("round checks never finished")
}

func TestRefineHalfwayTowardFailedMark(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseResident, offset: -10, fail: new(-50)})
	h.add(&journal.ProfileChange{To: []int{-10}})
	h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationStart, Steps: h.s.steps})
	h.add(&journal.GuardRotation{Rotation: 1, Event: journal.RotationEnd, Clean: true, Qualifying: true})
	r := nextRound(h)
	if diff := cmp.Diff([]int{-30}, r.Profile); diff != "" {
		t.Fatalf("first halfway (-want +got):\n%s", diff)
	}
	h.decide(h.next())
	h.decide(h.next())
	for range 30 {
		a := h.next()
		if a.Kind == RunTrial {
			h.trial(a, passed)
			continue
		}
		if p, ok := a.Payload.(*journal.RefineRound); ok && p.Event == journal.RotationEnd {
			h.decide(a)
			break
		}
		t.Fatalf("unexpected halfway action %+v", a)
	}
	next := nextRound(h)
	if diff := cmp.Diff([]int{-40}, next.Profile); diff != "" {
		t.Fatalf("second halfway (-want +got):\n%s", diff)
	}
}
