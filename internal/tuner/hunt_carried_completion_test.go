package tuner

import (
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestRunningHuntMaskCitesMixedCarriedAndLivePasses(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseDone, offset: -10}, coreStart{phase: journal.PhaseDone, offset: -10})
	d := h.s.durations.StartS
	first := carryTrials(h, machine.R7, []int{0, 1}, []int{-10, 0}, d, 3, journal.OutcomePass)
	second := carryTrials(h, machine.R7, []int{0, 1}, []int{0, -10}, d, 3, journal.OutcomePass)
	start := h.add(&journal.HuntStart{Hunt: 1, Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, DurationS: d, StartS: d, Starts: h.s.n, Failing: []int{-10, -10}, Anchor: []int{0, 0}, Candidates: []int{0, 1}})
	for mask, facts := range [][]int{first, second} {
		a, ok := h.s.huntNext()
		p, isMask := a.Payload.(*journal.HuntMask)
		if !ok || !isMask || p.Mask != mask+1 || p.Inferred != "" {
			t.Fatalf("mask %d should need two live starts: %+v", mask+1, a)
		}
		h.decide(a)
		for range 2 {
			a, ok = h.s.huntNext()
			if !ok || a.Kind != RunTrial || a.Trial.Mask != mask+1 {
				t.Fatalf("mask %d live start: %+v", mask+1, a)
			}
			h.trial(a, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: d})
		}
		a, ok = h.s.huntNext()
		if !ok || a.Kind != Decide {
			t.Fatalf("mask %d did not complete: %+v", mask+1, a)
		}
		want := append([]int{start.Seq}, first...)
		if mask == 1 {
			want = append(want, facts...)
			end, isEnd := a.Payload.(*journal.HuntEnd)
			if !isEnd || end.Result != "fallback" || end.Masks != 2 {
				t.Fatalf("completion should end the two-mask hunt without another fact: %+v", a)
			}
		} else if next, isNext := a.Payload.(*journal.HuntMask); !isNext || next.Mask != 2 {
			t.Fatalf("completion should plan mask 2 without another fact: %+v", a)
		}
		if diff := cmp.Diff(want, a.Cause); diff != "" {
			t.Fatalf("mask %d completion provenance (-want +got):\n%s", mask+1, diff)
		}
		if !strings.Contains(a.Payload.Message(), "20261002T004254Z") {
			t.Fatalf("mask %d completion omits source session: %s", mask+1, a.Payload.Message())
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
