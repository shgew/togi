package tuner

import (
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestDeepeningRechecksOwnPartialForEveryR7Workload(t *testing.T) {
	h := hasRoomHarness(t, -20, -20, -20, -20, -20, -20, -20, -20)
	initial := h.s.Profile()
	for _, w := range machine.Workloads(machine.R7) {
		h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "old", Seq: len(h.events) + 1}, Class: journal.TrialClass{Regime: machine.R7, Workload: w.ID, Cores: []int{0, 1, 2, 3}, DurationS: 120}, Profile: initial, Condition: machine.Together, Outcome: journal.OutcomePass, VoltageRequestsV: map[int]float64{0: 1.2, 1: 1.1, 2: 1.0, 3: .9}})
	}
	p := slices.Clone(initial)
	p[1]--
	h.add(&journal.DeepeningRound{Round: 2, Event: journal.LapStart, Profile: p, Target: p, Cores: []int{1}, Starts: 2, StartS: 120})
	checks := h.s.roundChecks()
	if len(checks) != 5 {
		t.Fatalf("checks = %+v, want R1/R2 and all three R7 workloads", checks)
	}
	for i, w := range machine.Workloads(machine.R7) {
		q := checks[i+2]
		if q.class.workload != w.ID || q.count != 2 || !slices.Equal(q.cores, []int{1, 2, 3}) {
			t.Fatalf("deepening checked containing parts instead of own partial: %+v", q)
		}
	}
	for range 4 {
		a := h.s.roundCheck()
		if a.Trial.Condition != machine.Alone || a.Trial.Core != 1 {
			t.Fatalf("solo deepening check not alone: %+v", a)
		}
		h.trial(a, passed)
	}
	for _, w := range machine.Workloads(machine.R7) {
		for range 2 {
			a := h.s.roundCheck()
			if a.Kind != RunTrial || a.Trial.Condition != machine.Together || a.Trial.Workload != w.ID || !slices.Equal(a.Trial.Cores, []int{1, 2, 3}) {
				t.Fatalf("R7 own partial check: %+v", a)
			}
			h.trial(a, passed)
		}
	}
	if end, ok := h.s.roundCheck().Payload.(*journal.DeepeningRound); !ok || !end.Passed {
		t.Fatal("alone and own-partial checks did not complete round")
	}
	assertProjectionReplay(h)
}

func TestDeepeningRequestRankChangeAddsCCDWholePart(t *testing.T) {
	h := hasRoomHarness(t, -10, -20, -30, -40, -20, -20, -20, -20)
	initial := h.s.Profile()
	for _, w := range machine.Workloads(machine.R7) {
		h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "old", Seq: len(h.events) + 1}, Class: journal.TrialClass{Regime: machine.R7, Workload: w.ID, Cores: []int{0, 1, 2, 3}, DurationS: 120}, Profile: initial, Condition: machine.Together, Outcome: journal.OutcomePass, VoltageRequestsV: map[int]float64{0: 1.2, 1: 1.1, 2: 1.0, 3: .9}})
	}
	p := slices.Clone(initial)
	p[0], p[1] = -50, -21
	h.add(&journal.DeepeningRound{Round: 1, Event: journal.LapStart, Profile: p, Target: p, Cores: []int{0, 1}, Starts: 2, StartS: 120})
	checks := h.s.roundChecks()
	var r7 [][]int
	for _, q := range checks {
		if q.class.regime == machine.R7 {
			r7 = append(r7, q.cores)
		}
	}
	want := [][]int{{0, 1, 2, 3}, {0, 2, 3}, {0, 1, 2, 3}, {0, 2, 3}, {0, 1, 2, 3}, {0, 2, 3}}
	if diff := cmp.Diff(want, r7); diff != "" {
		t.Fatalf("rank-change deepening checks (-want +got):\n%s", diff)
	}
	assertProjectionReplay(h)
}
