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

func TestHuntMaskReuseAfterReset(t *testing.T) {
	for _, stage := range []string{"part", "complement"} {
		for _, tt := range []struct {
			name       string
			pass       bool
			afterReset bool
			inferred   string
		}{
			{"old failure", false, false, ""},
			{"old pass", true, false, ""},
			{"new failure", false, true, "failure"},
			{"new pass", true, true, "pass"},
		} {
			t.Run(stage+"/"+tt.name, func(t *testing.T) {
				h := newHarness(t, searchAt(-30, -30, -30, -30)...)
				profile := []int{-30, -30, 0, 0}
				tr := Trial{Regime: machine.R7, Cores: h.s.ids(), Workload: machine.Workloads(machine.R7)[0].ID, DurationS: 120, Condition: machine.Masked, Profile: profile}
				reset := func() {
					h.add(&journal.MarkJoint{Mark: 1, Hunt: 1, Members: []journal.JointMember{{Core: 0, Offset: -30}, {Core: 1, Offset: -30}}})
					h.add(&journal.CommandReset{Core: new(0)})
					h.add(&journal.CorePhase{Core: 0, To: journal.PhaseSearch, Offset: -10, ClearedJoint: []int{1}})
				}
				if tt.afterReset {
					reset()
				}
				end, starts := passed, 5
				if !tt.pass {
					end, starts = journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash}, 1
				}
				for range starts {
					h.trial(Action{Kind: RunTrial, Trial: tr}, end)
				}
				if !tt.afterReset {
					reset()
				}
				h.add(&journal.HuntStart{Hunt: 2, Regime: tr.Regime, Workload: tr.Workload, Cores: tr.Cores, DurationS: 120, StartS: 120, Starts: 5, Failing: []int{-30, -30, -30, -30}, Anchor: []int{0, 0, 0, 0}, Candidates: h.s.ids()})
				a := h.s.planMask(h.s.hunt, maskPlan{set: h.s.ids(), cores: []int{0, 1}, g: 2, stage: stage, duration: 120}, "testing")
				m := a.Payload.(*journal.HuntMask)
				got := struct {
					Inferred string
					Skipped  bool
				}{m.Inferred, m.Skipped}
				want := struct {
					Inferred string
					Skipped  bool
				}{tt.inferred, false}
				if diff := cmp.Diff(want, got); diff != "" {
					t.Fatalf("mask after reset (-want +got):\n%s", diff)
				}
			})
		}
	}
}

func TestRunningHuntMaskReusesEarlierPasses(t *testing.T) {
	for _, stage := range []string{"part", "complement"} {
		t.Run(stage, func(t *testing.T) {
			h := newHarness(t, searchAt(-30, -30, -30, -30)...)
			tr := Trial{Regime: machine.R7, Cores: h.s.ids(), Workload: machine.Workloads(machine.R7)[0].ID, DurationS: 120, Condition: machine.Masked, Profile: []int{-30, -30, 0, 0}}
			for range 4 {
				h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
			}
			h.add(&journal.HuntStart{Hunt: 2, Regime: tr.Regime, Workload: tr.Workload, Cores: tr.Cores, DurationS: 120, StartS: 120, Starts: 5, Failing: []int{-30, -30, -30, -30}, Anchor: []int{0, 0, 0, 0}, Candidates: h.s.ids()})
			a := h.s.planMask(h.s.hunt, maskPlan{set: h.s.ids(), cores: []int{0, 1}, g: 2, stage: stage, duration: 120}, "testing")
			h.decide(a)
			for _, tt := range []struct {
				name    string
				passes  int
				outcome string
				advance bool
			}{
				{"before new start", 4, "running", false},
				{"after new start", 5, "pass", true},
			} {
				t.Run(tt.name, func(t *testing.T) {
					if tt.advance {
						tr.Hunt, tr.Mask = 2, 1
						h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
					}
					p := h.s.projectHunt()
					_, advance := h.s.nextMaskPlan(h.s.hunt)
					got := struct {
						Passes  int
						Outcome string
						Advance bool
					}{p.Masks[0].Passes, p.Masks[0].Outcome, advance}
					want := struct {
						Passes  int
						Outcome string
						Advance bool
					}{tt.passes, tt.outcome, tt.advance}
					if diff := cmp.Diff(want, got); diff != "" {
						t.Fatalf("running mask (-want +got):\n%s", diff)
					}
				})
			}
		})
	}
}

func TestHuntMaskDoesNotReuseShallowerOrIncomparablePasses(t *testing.T) {
	for _, tt := range []struct {
		name    string
		profile []int
	}{
		{"shallower", []int{-29, -30, 0, 0}},
		{"incomparable", []int{-31, -29, 0, 0}},
	} {
		t.Run(tt.name, func(t *testing.T) {
			h := newHarness(t, searchAt(-30, -30, -30, -30)...)
			tr := Trial{Regime: machine.R7, Cores: h.s.ids(), Workload: machine.Workloads(machine.R7)[0].ID, DurationS: 120, Condition: machine.Masked, Profile: tt.profile}
			for range 5 {
				h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
			}
			h.add(&journal.HuntStart{Hunt: 2, Regime: tr.Regime, Workload: tr.Workload, Cores: tr.Cores, DurationS: 120, StartS: 120, Starts: 5, Failing: []int{-30, -30, -30, -30}, Anchor: []int{0, 0, 0, 0}, Candidates: h.s.ids()})
			h.decide(h.s.planMask(h.s.hunt, maskPlan{set: h.s.ids(), cores: []int{0, 1}, g: 2, stage: "part", duration: 120}, "testing"))
			p := h.s.projectHunt()
			got := struct {
				Passes  int
				Outcome string
			}{p.Masks[0].Passes, p.Masks[0].Outcome}
			want := struct {
				Passes  int
				Outcome string
			}{0, "running"}
			if diff := cmp.Diff(want, got); diff != "" {
				t.Fatalf("mismatched profile (-want +got):\n%s", diff)
			}
		})
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

func TestHuntDurationPriorRespectsEvidenceAndShortFailures(t *testing.T) {
	for _, tt := range []struct {
		name               string
		count, workload    int
		profile            []int
		shortFailed, reset bool
		want               int
	}{
		{"valid deeper passes", 5, 0, []int{-32, -32}, false, false, 600},
		{"short failure in another workload", 5, 0, []int{-32, -32}, true, false, 120},
		{"insufficient starts", 4, 0, []int{-32, -32}, false, false, 120},
		{"different workload", 5, 1, []int{-32, -32}, false, false, 120},
		{"incomparable profile", 5, 0, []int{-32, -29}, false, false, 120},
		{"reset evidence boundary", 5, 0, []int{-32, -32}, false, true, 120},
	} {
		t.Run(tt.name, func(t *testing.T) {
			h := newHarness(t, coreStart{phase: journal.PhaseDone, offset: -30}, coreStart{phase: journal.PhaseDone, offset: -30})
			h.decide(h.next())
			tr := Trial{Regime: machine.R7, Cores: h.s.ids(), Workload: machine.Workloads(machine.R7)[tt.workload].ID, Condition: machine.Masked, Phase: journal.PhaseHunt, DurationS: 120, Profile: tt.profile}
			if tt.shortFailed {
				bad := tr
				bad.Workload, bad.Profile = machine.Workloads(machine.R7)[1].ID, []int{-40, 0}
				h.trial(Action{Kind: RunTrial, Trial: bad}, failed)
				h.decide(h.next())
				h.decide(h.next())
			}
			var seqs []int
			for range tt.count {
				_, end := h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
				seqs = append(seqs, end.Seq)
			}
			if tt.reset {
				h.add(&journal.CommandReset{Core: new(0)})
			}
			tr.Workload = machine.Workloads(machine.R7)[0].ID
			tr.DurationS, tr.Profile, tr.Condition, tr.Phase = 600, []int{-30, -30}, machine.Resident, journal.PhaseGuard
			h.trial(Action{Kind: RunTrial, Trial: tr}, failed)
			h.decide(h.next())
			h.decide(h.s.huntStartNext())
			plan, ok := h.s.nextMaskPlan(h.s.hunt)
			if !ok || plan.duration != tt.want {
				t.Fatalf("initial hunt duration: %+v, want %d", plan, tt.want)
			}
			a := h.s.planMask(h.s.hunt, plan, "partition")
			if tt.want == 600 {
				if diff := cmp.Diff(append([]int{h.s.hunt.seq}, seqs...), a.Cause); diff != "" {
					t.Fatalf("duration evidence cause (-want +got):\n%s", diff)
				}
			}
			h.decide(a)
			next, _ := h.s.huntNext()
			if a.Payload.(*journal.HuntMask).Inferred == "" && (next.Kind != RunTrial || next.Trial.DurationS != tt.want) {
				t.Fatalf("masked trial duration: %+v", next)
			}
			replayed := New()
			var state journal.State
			journal.Replay(h.events, &state, replayed)
			replayNext, _ := replayed.huntNext()
			if diff := cmp.Diff(next, replayNext); diff != "" {
				t.Fatalf("duration resume (-live +replayed):\n%s", diff)
			}
		})
	}
}

func TestRepeatedMaskedCoreProbeReturnsToBinaryPartsAfterPass(t *testing.T) {
	starts := make([]coreStart, 16)
	for i := range starts {
		starts[i] = coreStart{phase: journal.PhaseDone, offset: -10, fail: new(-11)}
	}
	h := newHarness(t, starts...)
	var prior []int
	for range 1000 {
		a := h.next()
		if m, ok := a.Payload.(*journal.HuntMask); ok && m.Hunt == 3 && m.Mask == 1 {
			if diff := cmp.Diff([]int{3}, m.Cores); diff != "" {
				t.Fatalf("corroborated core was not probed first (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff(append([]int{h.s.hunt.seq}, prior...), a.Cause); diff != "" {
				t.Fatalf("probe lost its evidence (-want +got):\n%s", diff)
			}
			h.decide(a)
			for range h.s.n {
				trial := h.next()
				h.trial(trial, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: trial.Trial.DurationS})
			}
			normal := h.next()
			p, ok := normal.Payload.(*journal.HuntMask)
			if !ok || p.Granularity != 2 || p.Index != 0 || p.Inferred != "" || p.Profile[3] != -8 || p.Profile[4] != -10 {
				t.Fatalf("a passing singleton must not establish its untested binary group: %+v", normal)
			}
			replayed := New()
			for _, e := range h.events {
				replayed.Fold(e)
			}
			if diff := cmp.Diff(normal, replayed.Next()); diff != "" {
				t.Fatalf("resumed probe fallback changed (-want +got):\n%s", diff)
			}
			return
		}
		if a.Kind == Decide {
			e := h.decide(a)
			if f, ok := e.Data.(*journal.Failure); ok && f.Condition == machine.Masked && f.Attribution == journal.Attributed && f.Core != nil && *f.Core == 3 {
				prior = append(prior, e.Seq)
			}
			continue
		}
		if a.Kind != RunTrial {
			t.Fatalf("unexpected action: %+v", a)
		}
		intent := h.start(a)
		p := intent.Data.(*journal.TrialIntent)
		end := &journal.TrialEnd{Trial: p.Trial, Outcome: journal.OutcomePass, DurationS: p.DurationS}
		if p.Profile[3] < -8 || p.Profile[3] < 0 && p.Profile[4] < -9 {
			end.Outcome, end.Signal = journal.OutcomeFailure, machine.Crash
		}
		h.add(end, intent.Seq)
	}
	t.Fatal("corroborated singleton probe was never scheduled")
}

func TestRepeatedMaskedCoreProbeRequiresMatchingAdjacentFailuresSinceReset(t *testing.T) {
	for _, tt := range []struct {
		name   string
		change func(*Trial, *journal.Failure)
		reset  bool
		probe  bool
	}{
		{name: "matching adjacent failures", probe: true},
		{name: "different workload", change: func(tr *Trial, _ *journal.Failure) { tr.Workload = machine.Workloads(machine.R7)[1].ID }},
		{name: "different duration", change: func(tr *Trial, _ *journal.Failure) { tr.DurationS = 600 }},
		{name: "different loaded cores", change: func(tr *Trial, _ *journal.Failure) { tr.Cores = []int{0, 1} }},
		{name: "resident failure", change: func(tr *Trial, f *journal.Failure) { tr.Condition, f.Condition = machine.Resident, machine.Resident }},
		{name: "unattributed failure", change: func(_ *Trial, f *journal.Failure) { f.Attribution, f.Core, f.Offset = journal.Unattributed, nil, nil }},
		{name: "different culprit", change: func(_ *Trial, f *journal.Failure) { f.Core = new(0) }},
		{name: "nonadjacent offsets", change: func(_ *Trial, f *journal.Failure) { f.Offset = new(-8) }},
		{name: "reset between failures", reset: true},
	} {
		t.Run(tt.name, func(t *testing.T) {
			starts := make([]coreStart, 4)
			for i, offset := range []int{-20, -8, -20, -20} {
				starts[i] = coreStart{phase: journal.PhaseDone, offset: offset}
			}
			h := newHarness(t, starts...)
			h.decide(h.next())
			var prior []int
			for i, offset := range []int{-10, -9} {
				if i == 1 && tt.reset {
					h.add(&journal.CommandReset{Core: new(1)})
				}
				tr := Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: h.s.ids(), Condition: machine.Masked, Phase: journal.PhaseHunt, DurationS: 120, Profile: []int{0, offset, 0, 0}}
				failure := &journal.Failure{Attribution: journal.Attributed, Core: new(1), Offset: new(offset), Condition: machine.Masked, Regime: machine.R7, Profile: tr.Profile}
				if i == 1 && tt.change != nil {
					tt.change(&tr, failure)
				}
				intent, end := h.trial(Action{Kind: RunTrial, Trial: tr}, failed)
				failure.Trial = intent.Data.(*journal.TrialIntent).Trial
				prior = append(prior, h.add(failure, end.Seq).Seq)
			}
			h.add(&journal.CorePhase{Core: 1, To: journal.PhaseDone, Offset: -8, FailedMark: new(-9)})
			tr := Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: h.s.ids(), Condition: machine.Resident, Phase: journal.PhaseGuard, DurationS: 120, Profile: []int{-20, -8, -20, -20}}
			intent, end := h.trial(Action{Kind: RunTrial, Trial: tr}, failed)
			failure := h.add(&journal.Failure{Trial: intent.Data.(*journal.TrialIntent).Trial, Attribution: journal.Unattributed, Condition: machine.Resident, Regime: machine.R7, Profile: tr.Profile}, end.Seq)
			h.decide(h.s.huntStartNext())
			plan, ok := h.s.nextMaskPlan(h.s.hunt)
			if !ok {
				t.Fatal("hunt did not schedule a mask")
			}
			want := []int{0, 1}
			if tt.probe {
				want = []int{1}
			}
			if diff := cmp.Diff(want, plan.cores); diff != "" {
				t.Fatalf("initial mask for failure #%d (-want +got):\n%s", failure.Seq, diff)
			}
			if tt.probe {
				action := h.s.planMask(h.s.hunt, plan, "partition")
				if diff := cmp.Diff(append([]int{h.s.hunt.seq}, prior...), action.Cause); diff != "" {
					t.Fatalf("probe cause (-want +got):\n%s", diff)
				}
			}
		})
	}
}

func TestActiveHuntProjection(t *testing.T) {
	for _, tc := range []struct {
		name      string
		stage     string
		prior     int
		newPasses int
		fail      bool
		inferred  string
		skipped   bool
		reset     bool
		escalated bool
		edge      *journal.JointMember
		held      []journal.JointMember
		passes    int
		outcome   string
	}{
		{name: "running reuses earlier valid passes", stage: "part", prior: 2, newPasses: 1, passes: 3, outcome: "running"},
		{name: "passing complement accumulates starts", stage: "complement", prior: 4, newPasses: 1, passes: 5, outcome: "pass"},
		{name: "failure invalidates earlier passes", stage: "part", prior: 2, fail: true, outcome: "failure"},
		{name: "inferred pass", stage: "part", prior: 5, inferred: "pass", passes: 5, outcome: "pass"},
		{name: "inferred failure", stage: "complement", inferred: "failure", outcome: "failure"},
		{name: "skipped", stage: "part", skipped: true, outcome: "skipped"},
		{name: "reset excludes reused evidence", stage: "part", prior: 4, reset: true, newPasses: 1, passes: 1, outcome: "running"},
		{name: "escalated full uses only new starts", stage: "full", prior: 4, newPasses: 1, escalated: true, passes: 1, outcome: "running"},
		{name: "edge preserves held members", stage: "edge", prior: 4, newPasses: 1, edge: &journal.JointMember{Core: 0, Offset: -30}, held: []journal.JointMember{{Core: 1, Offset: -30}}, passes: 1, outcome: "running"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := huntHarness(t, 4, 600)
			old := h.s.hunt.start
			duration := 120
			if tc.escalated {
				duration = 600
			}
			tr := Trial{Regime: old.Regime, Workload: old.Workload, Cores: old.Cores, Condition: machine.Masked, DurationS: duration, Profile: []int{-30, -30, 0, 0}, Hunt: 2, Mask: 1}
			for range tc.prior {
				h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
			}
			if tc.reset {
				h.add(&journal.CommandReset{Core: new(0)})
			}
			start := &journal.HuntStart{Hunt: 2, Failure: old.Failure, Trial: old.Trial, Regime: old.Regime, Workload: old.Workload, Cores: old.Cores, DurationS: 600, Starts: 5, StartS: 120, Anchor: []int{0, 0, 0, 0}, Failing: []int{-30, -30, -30, -30}, Candidates: h.s.ids()}
			begin := h.add(start)
			mask := h.add(&journal.HuntMask{Hunt: 2, Mask: 1, Cores: []int{0, 1}, Profile: tr.Profile, Stage: tc.stage, DurationS: duration, Inferred: tc.inferred, Skipped: tc.skipped, Escalated: tc.escalated, Edge: tc.edge, Held: tc.held})
			for range tc.newPasses {
				h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
			}
			if tc.fail {
				h.trial(Action{Kind: RunTrial, Trial: tr}, failed)
			}
			want := &journal.HuntState{Hunt: 2, Seq: begin.Seq, Failure: start.Failure, Regime: start.Regime, Trial: start.Trial, Anchor: start.Anchor, Candidates: start.Candidates, Escalated: tc.escalated, Masks: []journal.MaskState{{Mask: 1, Seq: mask.Seq, Cores: []int{0, 1}, Edge: tc.edge, Held: tc.held, Outcome: tc.outcome, Passes: tc.passes, Needed: 5}}}
			st := projected(h)
			if st.Phase != string(journal.PhaseHunt) {
				t.Fatalf("active hunt phase %s", st.Phase)
			}
			if diff := cmp.Diff(want, st.Hunt); diff != "" {
				t.Fatalf("hunt projection (-want +got):\n%s", diff)
			}
			assertProjectionReplay(h)
			h.add(&journal.HuntEnd{Hunt: 2, Result: "cancelled"}, begin.Seq)
			if projected(h).Hunt != nil {
				t.Fatal("cancelled hunt remains active")
			}
			assertProjectionReplay(h)
		})
	}
}

func TestHuntSkipsPreviouslyMarkedFailure(t *testing.T) {
	h := huntHarness(t, 2, 120)
	failure := h.add(&journal.Failure{Attribution: journal.Unattributed, Condition: machine.Resident, Signal: machine.Crash, Profile: h.s.Profile()})
	for range 2 {
		h.decide(h.next())
		for range h.s.n {
			runMask(h, h.next(), false)
		}
	}
	h.decide(h.next())
	h.decide(h.next())
	h.decide(h.next())
	h.add(&journal.ProfileChange{From: h.s.Profile(), To: h.s.offsets()})
	a := h.s.huntStartNext()
	p, ok := a.Payload.(*journal.HuntSkipped)
	if !ok || p.Failure != failure.Seq || !strings.Contains(p.Reason, "joint mark J1") || cmp.Diff([]int{failure.Seq}, a.Cause) != "" {
		t.Fatalf("marked source was hunted: %+v", a)
	}
	h.decide(a)
	if len(h.s.queue) != 0 || h.s.hunt != nil {
		t.Fatal("skipped source remains pending")
	}
}

func TestDrainCommitsResolvedHuntWithoutStartingMasks(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseDone, offset: -30, fail: new(-31)}, coreStart{phase: journal.PhaseDone, offset: -30, fail: new(-31)})
	h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "old", Trial: "source"}, Class: journal.TrialClass{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, DurationS: 120}, Condition: machine.Masked, Core: new(1), Profile: []int{0, -30}, Outcome: journal.OutcomeFailure, Signal: machine.Crash})
	h.decide(h.next())
	h.trial(Action{Kind: RunTrial, Trial: Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, Condition: machine.Resident, Phase: journal.PhaseGuard, DurationS: 600}}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash})
	h.decide(h.next())
	h.decide(h.next())
	if a, ok := h.s.Drain(); ok {
		t.Fatalf("drain planned new mask: %+v", a)
	}
	h.decide(h.next())
	a, ok := h.s.Drain()
	end, yes := a.Payload.(*journal.HuntEnd)
	if !ok || !yes || end.Result != "culprit" || cmp.Diff([]int{1}, end.Cores) != "" {
		t.Fatalf("drain did not resolve singleton: %+v", a)
	}
	h.decide(a)
	a, ok = h.s.Drain()
	back, yes := a.Payload.(*journal.TunerDecision)
	if !ok || !yes || back.Decision != journal.Backoff || back.Core != 1 || back.ToOffset != -29 {
		t.Fatalf("drain did not commit culprit: %+v", a)
	}
	h.decide(a)
	if a, ok := h.s.Drain(); ok {
		t.Fatalf("drain ran work after commitment: %+v", a)
	}
}

func TestDrainRecordsFallbackJointBeforeBackoff(t *testing.T) {
	h := huntHarness(t, 2, 120)
	for range 2 {
		h.decide(h.next())
		for range h.s.n {
			runMask(h, h.next(), false)
		}
	}
	a, ok := h.s.Drain()
	end, yes := a.Payload.(*journal.HuntEnd)
	if !ok || !yes || end.Result != "fallback" {
		t.Fatalf("drain did not close unresolved hunt: %+v", a)
	}
	h.decide(a)
	a, ok = h.s.Drain()
	mark, yes := a.Payload.(*journal.MarkJoint)
	if !ok || !yes || !mark.Fallback || cmp.Diff([]journal.JointMember{{Core: 0, Offset: -30}, {Core: 1, Offset: -30}}, mark.Members) != "" {
		t.Fatalf("drain lost fallback mark: %+v", a)
	}
	h.decide(a)
	a, ok = h.s.Drain()
	back, yes := a.Payload.(*journal.TunerDecision)
	if !ok || !yes || back.Decision != journal.Backoff || back.ToOffset != -29 {
		t.Fatalf("drain did not break joint: %+v", a)
	}
	h.decide(a)
	if _, reached := h.s.Reaches(h.s.offsets()); reached {
		t.Fatal("drain left resident offsets reaching joint")
	}
}
