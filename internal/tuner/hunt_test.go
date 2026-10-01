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

func huntHarness(t *testing.T, cores int, duration int) *harness {
	t.Helper()
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

func TestHuntAnchorRaisesTheQualifiedProfile(t *testing.T) {
	for _, tt := range []struct {
		name               string
		qualified, failing []int
		anchor             []int
		anchorSeq          int
		candidates         []int
	}{
		{"shallower everywhere", []int{-10, -10, -10, -10}, []int{-12, -10, -10, -10}, []int{-10, -10, -10, -10}, 7, []int{0}},
		{"a yielded core takes its failing offset", []int{-10, -10, -10, -10}, []int{-12, -8, -10, -10}, []int{-10, -8, -10, -10}, 7, []int{0}},
		{"deeper everywhere falls back to all-zero", []int{-12, -12, -12, -12}, []int{-10, -10, -10, -10}, []int{0, 0, 0, 0}, 0, []int{0, 1, 2, 3}},
	} {
		t.Run(tt.name, func(t *testing.T) {
			starts := make([]coreStart, 4)
			for i := range starts {
				starts[i] = coreStart{phase: journal.PhaseDone, offset: tt.failing[i]}
			}
			h := newHarness(t, starts...)
			h.s.qualified = []qualified{{profile: tt.qualified, seq: 7}}
			class := trialClass{machine.R7, machine.Workloads(machine.R7)[0].ID, fmt.Sprint(h.s.ids()), 120}
			h.s.queue = []pendingFailure{{seq: 9, failure: &journal.Failure{Trial: "0001"}, profile: tt.failing, class: class}}
			p, ok := h.s.huntStartNext().Payload.(*journal.HuntStart)
			if !ok {
				t.Fatal("no hunt.start")
			}
			got := struct {
				Anchor     []int
				AnchorSeq  int
				Candidates []int
			}{p.Anchor, p.AnchorSeq, p.Candidates}
			want := struct {
				Anchor     []int
				AnchorSeq  int
				Candidates []int
			}{tt.anchor, tt.anchorSeq, tt.candidates}
			if diff := cmp.Diff(want, got); diff != "" {
				t.Fatalf("hunt.start (-want +got):\n%s", diff)
			}
		})
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
	var probes []journal.JointMember
	var end *journal.HuntEnd
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
			if mask.Edge != nil {
				probes = append(probes, *mask.Edge)
			}
			continue
		}
		if a.Kind == RunTrial {
			p := a.Trial.Profile
			runMask(h, a, p[0] <= -30 && p[1] <= -20 && p[2] == 0 && p[3] == 0)
			continue
		}
		if e, ok := a.Payload.(*journal.HuntEnd); ok {
			end = e
			h.decide(a)
			break
		}
		t.Fatalf("unexpected action %+v", a)
	}
	if end == nil || end.Result != "joint" || cmp.Diff([]int{0, 1}, end.Cores) != "" {
		t.Fatalf("hunt result %+v", end)
	}
	if diff := cmp.Diff([]journal.JointMember{{Core: 0, Offset: -30}, {Core: 1, Offset: -20}}, end.Members); diff != "" {
		t.Errorf("probed members (-want +got):\n%s", diff)
	}
	want := []journal.JointMember{{Core: 0, Offset: -29}}
	for _, offset := range []int{-29, -28, -26, -22, -14, -18, -20, -19} {
		want = append(want, journal.JointMember{Core: 1, Offset: offset})
	}
	if diff := cmp.Diff(want, probes); diff != "" {
		t.Errorf("edge probes (-want +got):\n%s", diff)
	}
	mark := h.next()
	p, ok := mark.Payload.(*journal.MarkJoint)
	if !ok || cmp.Diff([]journal.JointMember{{Core: 0, Offset: -30}, {Core: 1, Offset: -20}}, p.Members) != "" {
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
	if !ok || d.Decision != journal.Backoff || d.Phase != journal.PhaseHunt || d.Core != 0 || d.ToOffset != -29 {
		t.Fatalf("commitment %+v", d)
	}
	h.decide(back)
	if h.s.hunt != nil {
		t.Fatal("hunt reopened after commitment")
	}
	if _, reached := h.s.Reaches(h.s.offsets()); reached {
		t.Fatalf("commitment reached mark: %v", h.s.offsets())
	}
}

func TestJointBackoffMovesToATestedProbe(t *testing.T) {
	h := huntHarness(t, 4, 120)
	fails := func(p []int) bool {
		return p[0] <= -27 && p[1] <= -27 || p[0] == -26 && p[1] == -30
	}
	probes := map[int]int{}
	for range 200 {
		a := h.next()
		if mask, ok := a.Payload.(*journal.HuntMask); ok {
			e := h.decide(a)
			if mask.Edge != nil {
				probes[e.Seq] = mask.Edge.Offset
			}
			continue
		}
		if a.Kind == RunTrial {
			runMask(h, a, fails(a.Trial.Profile))
			continue
		}
		if _, ok := a.Payload.(*journal.HuntEnd); ok {
			h.decide(a)
			break
		}
		t.Fatalf("unexpected action %+v", a)
	}
	mark := h.next()
	p, ok := mark.Payload.(*journal.MarkJoint)
	if !ok || cmp.Diff([]journal.JointMember{{Core: 0, Offset: -26}, {Core: 1, Offset: -30}}, p.Members) != "" {
		t.Fatalf("mark %+v", mark)
	}
	h.decide(mark)
	back := h.next()
	d, ok := back.Payload.(*journal.TunerDecision)
	if !ok || d.Decision != journal.Backoff || d.Core != 0 || d.ToOffset != -25 {
		t.Fatalf("commitment %+v, want core 00 to -25, where its probe passed with core 01 at -30", back)
	}
	if len(back.Cause) != 2 || probes[back.Cause[1]] != -25 {
		t.Fatalf("commitment cause %v, want the joint mark and the passing probe", back.Cause)
	}
	h.decide(back)
	if fails(h.s.offsets()) {
		t.Fatalf("resident %v still fails; one count on core 01 would have left it failing", h.s.offsets())
	}
}

func TestNextHuntReusesEstablishedPartMasks(t *testing.T) {
	h := huntHarness(t, 4, 120)
	fails := func(p []int) bool { return p[0] <= -28 && p[1] <= -28 }
	var first []*journal.HuntMask
	var second []*journal.HuntMask
	hunts := 1
	for range 400 {
		a := h.next()
		switch p := a.Payload.(type) {
		case *journal.HuntStart:
			hunts++
			h.decide(a)
			continue
		case *journal.HuntMask:
			if hunts == 1 {
				first = append(first, p)
			} else {
				second = append(second, p)
			}
			h.decide(a)
			continue
		}
		if a.Kind == RunTrial && a.Trial.Rerun {
			h.trial(a, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 5})
			continue
		}
		if a.Kind == RunTrial {
			if hunts == 2 {
				break
			}
			runMask(h, a, fails(a.Trial.Profile))
			continue
		}
		h.decide(a)
	}
	if hunts != 2 {
		t.Fatalf("hunts %d, want a second hunt after the failed rerun", hunts)
	}
	ran := func(m *journal.HuntMask) bool { return m.Inferred == "" && !m.Skipped }
	idle := slices.IndexFunc(first, func(m *journal.HuntMask) bool { return slices.Equal(m.Cores, []int{2, 3}) && ran(m) })
	if idle < 0 {
		t.Fatalf("first hunt never ran the part of cores 02 and 03: %+v", first)
	}
	again := slices.IndexFunc(second, func(m *journal.HuntMask) bool { return slices.Equal(m.Cores, []int{2, 3}) })
	if again < 0 || second[again].Inferred != "pass" || !slices.Equal(second[again].Profile, first[idle].Profile) {
		t.Fatalf("second hunt masks %+v, want the part of cores 02 and 03 inferred from the first hunt's passes", second)
	}
}

func TestHuntFallbackAndFullCheck(t *testing.T) {
	for _, tc := range []struct {
		name      string
		duration  int
		fullFails bool
		result    string
		reason    string
	}{
		{"fallback", 120, false, "fallback", "no tested mask failed, so the remaining candidates stay unresolved and are marked together"},
		{"full failure", 600, true, "joint", "masked trial outcomes isolated the minimal failing set; edge probes found it still failing at core 00 -30 + core 01 -30 and passing with any one member a count shallower"},
	} {
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
					if end.Result != tc.result || end.Reason != tc.reason {
						t.Fatalf("result %s (%s), want %s (%s)", end.Result, end.Reason, tc.result, tc.reason)
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

func TestHuntCulpritDiscardsContradictedPass(t *testing.T) {
	h := residentHarness(t, -29, -30)
	h.add(&journal.HuntStart{Hunt: 1, Failure: 10, Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, DurationS: 120, Failing: []int{-29, -30}, Anchor: []int{0, 0}, Candidates: []int{0, 1}, Starts: 5, StartS: 120})
	h.add(&journal.HuntEnd{Hunt: 1, Result: "culprit", Cores: []int{0}, Masks: 2})
	a := h.next()
	d, ok := a.Payload.(*journal.TunerDecision)
	if !ok || d.Phase != journal.PhaseHunt || d.Core != 0 || d.Pass != nil || d.FailedMark == nil || *d.FailedMark != -29 {
		t.Fatalf("culprit commitment kept contradicted pass: %+v", a)
	}
	if !strings.Contains(d.Reason, "passed step at -29 discarded, the failure contradicts it") {
		t.Fatalf("culprit commitment reason: %q", d.Reason)
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
