package tuner

import (
	"fmt"
	"testing"

	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func r7Infos(ids ...int) []machine.CoreInfo {
	infos := make([]machine.CoreInfo, len(ids))
	for i, id := range ids {
		infos[i] = machine.CoreInfo{Core: id, CCD: 0, CPUs: []int{id, id + 32}}
	}
	return infos
}

func carriedR7Pass(h *harness, cores, profile, top []int) journal.Event {
	return h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "old", Seq: len(h.events) + 1, Trial: fmt.Sprint(len(h.events)), Evidence: EvidenceEpoch}, Class: journal.TrialClass{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: cores, DurationS: 120}, Condition: machine.Together, Profile: profile, Outcome: journal.OutcomePass, DurationS: 120, TopRequesters: top})
}

func TestR7StatusSeparatesWorkloadsOffsetsAndTopRequesters(t *testing.T) {
	h := newHarnessOn(t, r7Infos(3, 7), config.Default(), coreStart{phase: journal.PhaseAtLimit, offset: -20}, coreStart{phase: journal.PhaseAtLimit, offset: -30})
	workload := machine.Workloads(machine.R7)[0].ID
	carriedR7Pass(h, []int{3, 7}, []int{-21, -30}, []int{3})
	carriedR7Pass(h, []int{3, 7}, []int{-19, -29}, []int{7})
	for _, status := range h.s.R7Status() {
		want := status.Workload == workload && status.Core == 3
		if status.SelfSufficient != want {
			t.Fatalf("self-sufficiency crosses workload, offset or top requester: %+v", status)
		}
		if status.TopRequester != (status.Core == 3) || !status.OffsetFallback {
			t.Fatalf("current offset fallback order: %+v", status)
		}
	}
	h.add(&journal.CommandReset{Core: new(3)})
	for _, status := range h.s.R7Status() {
		if status.SelfSufficient || status.Passes != 0 {
			t.Fatalf("reset core retains self-sufficiency: %+v", status)
		}
	}
}

func TestR7StatusIgnoresFailedStartsAsTopRequester(t *testing.T) {
	for _, carried := range []bool{false, true} {
		t.Run(map[bool]string{false: "live", true: "carried"}[carried], func(t *testing.T) {
			h := r7Harness(t)
			workload := machine.Workloads(machine.R7)[0].ID
			if carried {
				r7Fact(h, false, []int{0, 1}, h.s.Profile(), map[int]float64{0: 1.1, 1: 1.08}, []int{0}, nil, nil, nil)
			} else {
				tr := Trial{Regime: machine.R7, Workload: workload, Cores: []int{0, 1}, DurationS: 120, Phase: journal.PhaseChecking, Condition: machine.Together, Profile: h.s.Profile()}
				h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, DurationS: 41, TopRequesters: []int{0}})
			}
			for _, status := range h.s.R7Status() {
				if status.Workload == workload && status.Core == 0 && (status.Passes != 0 || status.SelfSufficient) {
					t.Fatalf("a failed trial as top requester counted as self-sufficiency: %+v", status)
				}
			}
		})
	}
}

func TestR7StatusUsesSortedProfileAndOnlyCCDParts(t *testing.T) {
	infos := []machine.CoreInfo{{Core: 7, CCD: 1}, {Core: 3, CCD: 0}, {Core: 9, CCD: 1}, {Core: 5, CCD: 0}}
	h := newHarnessOn(t, infos, config.Default(),
		coreStart{phase: journal.PhaseAtLimit, offset: -25}, coreStart{phase: journal.PhaseAtLimit, offset: -20},
		coreStart{phase: journal.PhaseAtLimit, offset: -35}, coreStart{phase: journal.PhaseAtLimit, offset: -30})
	statuses := h.s.R7Status()
	if len(statuses) != 4*len(machine.Workloads(machine.R7)) {
		t.Fatalf("all-core part duplicated status rows: %d", len(statuses))
	}
	seen := map[string]map[int]bool{}
	for _, status := range statuses {
		if seen[status.Workload] == nil {
			seen[status.Workload] = map[int]bool{}
		}
		if seen[status.Workload][status.Core] {
			t.Fatalf("duplicate core/workload: %+v", status)
		}
		seen[status.Workload][status.Core] = true
		if status.TopRequester != (status.Core == 3 || status.Core == 7) {
			t.Fatalf("request order used enumeration rather than core-id profile: %+v", status)
		}
	}
}

// A journal always starts with its topology; this is the nearest input, a fact folded before any SessionStart.
func TestR7StatusWithoutTopology(t *testing.T) {
	h := bareHarness(t)
	carriedR7Pass(h, []int{3, 7}, []int{-20, -30}, nil)
	if got := h.s.R7Status(); len(got) != 0 {
		t.Fatalf("status invented cores without topology: %+v", got)
	}
}
