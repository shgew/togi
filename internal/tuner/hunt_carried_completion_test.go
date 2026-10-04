package tuner

import (
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestRunningHuntGroupCitesMixedCarriedAndLivePasses(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -10}, coreStart{phase: journal.PhaseAtLimit, offset: -10})
	d := h.s.durations.ShortTrialS
	first := carryTrials(h, machine.R7, []int{0, 1}, []int{-10, 0}, d, 3, journal.OutcomePass)
	second := carryTrials(h, machine.R7, []int{0, 1}, []int{0, -10}, d, 3, journal.OutcomePass)
	start := h.add(&journal.HuntStart{Hunt: 1, Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, DurationS: d, TrialS: d, Trials: h.s.n, Failing: []int{-10, -10}, Parked: []int{0, 0}, Candidates: []int{0, 1}})
	for group, facts := range [][]int{first, second} {
		a, ok := h.s.huntNext()
		p, isGroup := a.Payload.(*journal.HuntGroup)
		if !ok || !isGroup || p.Group != group+1 || p.Inferred != "" {
			t.Fatalf("group %d should need two live trials: %+v", group+1, a)
		}
		h.decide(a)
		for range 2 {
			a, ok = h.s.huntNext()
			if !ok || a.Kind != RunTrial || a.Trial.Group != group+1 {
				t.Fatalf("group %d live trial: %+v", group+1, a)
			}
			h.trial(a, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: d})
		}
		a, ok = h.s.huntNext()
		if !ok || a.Kind != Decide {
			t.Fatalf("group %d did not complete: %+v", group+1, a)
		}
		want := append([]int{start.Seq}, first...)
		if group == 1 {
			want = append(want, facts...)
			end, isEnd := a.Payload.(*journal.HuntEnd)
			if !isEnd || end.Result != "fallback" || end.Groups != 2 {
				t.Fatalf("completion should end the two-group hunt without another fact: %+v", a)
			}
		} else if next, isNext := a.Payload.(*journal.HuntGroup); !isNext || next.Group != 2 {
			t.Fatalf("completion should plan group 2 without another fact: %+v", a)
		}
		if diff := cmp.Diff(want, a.Cause); diff != "" {
			t.Fatalf("group %d completion provenance (-want +got):\n%s", group+1, diff)
		}
		if !strings.Contains(a.Payload.Message(), "20261002T004254Z") {
			t.Fatalf("group %d completion omits source session: %s", group+1, a.Payload.Message())
		}
		replayed := New()
		for _, e := range h.events {
			replayed.Fold(e)
		}
		fromReplay, replayOK := replayed.huntNext()
		if !replayOK || cmp.Diff(a, fromReplay) != "" {
			t.Fatalf("completion changed after replay: %+v", fromReplay)
		}
	}
}

func TestHuntPlanMarksOnlyGroupsAnsweredByCarriedEvidence(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -10}, coreStart{phase: journal.PhaseAtLimit, offset: -10})
	d := h.s.durations.ShortTrialS
	carryTrials(h, machine.R7, []int{0, 1}, []int{-10, 0}, d, h.s.n, journal.OutcomePass)
	h.add(&journal.HuntStart{Hunt: 1, Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, DurationS: d, TrialS: d, Trials: h.s.n, Failing: []int{-10, -10}, Parked: []int{0, 0}, Candidates: []int{0, 1}})
	for range 20 {
		a, ok := h.s.huntNext()
		if !ok || a.Kind == Decide && a.Payload.Kind() == journal.KindHuntEnd {
			break
		}
		if a.Kind == RunTrial {
			h.trial(a, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: d})
			continue
		}
		h.decide(a)
	}
	groups := h.s.HuntPlan().Groups
	if len(groups) != 2 || groups[0].Outcome != "pass" || !groups[0].Carried || groups[1].Outcome != "pass" || groups[1].Carried {
		t.Fatalf("group 1 is answered by carried trials and group 2 by live ones: %+v", groups)
	}
}
