package tuner

import (
	"fmt"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

const (
	mprimeOld    = "/nix/store/64hjzgj1msiyndpdxrk9l3gkjf3sczgj-mprime-31.04b02"
	mprimeNew    = "/nix/store/0000000000000000000000000000000a-mprime-31.04b03"
	ycruncherOld = "/nix/store/n5g91xa9pzcfqyyh62xpwz3v53f87y0a-y-cruncher-0.8.7.9547"
	ycruncherNew = "/nix/store/0000000000000000000000000000000b-y-cruncher-0.8.7.9548"
)

func loadBackends(h *harness, mprime, ycruncher string) {
	h.t.Helper()
	c := snapshotConfig(config.Default())
	c.Backends = journal.ConfigBackends{Mprime: mprime, Ycruncher: ycruncher}
	h.add(&journal.ConfigLoaded{Path: config.DefaultPath, Config: c})
}

func liveTrials(h *harness, w machine.Workload, profile []int, count int, end journal.TrialEnd) {
	h.t.Helper()
	for range count {
		h.trials++
		id := fmt.Sprintf("%04d", h.trials)
		intent := h.add(&journal.TrialIntent{Trial: id, Regime: machine.R1, Workload: w.ID, Cores: []int{0}, DurationS: 90, Condition: machine.Together, Profile: profile})
		end.Trial = id
		h.add(&end, intent.Seq)
	}
}

func TestEvidenceIsKeyedByBackendStorePath(t *testing.T) {
	r1 := machine.Workloads(machine.R1)
	mprime, ycruncher := r1[0], r1[1]
	if mprime.Backend != machine.Mprime || ycruncher.Backend != machine.Ycruncher {
		t.Fatalf("R1 workloads %s and %s no longer run mprime and y-cruncher", mprime.ID, ycruncher.ID)
	}
	profile := []int{-10, -10}
	classOfWorkload := func(w machine.Workload) trialClass { return trialClass{machine.R1, w.ID, "[0]", 90} }
	for _, tc := range []struct {
		name               string
		reload             [][2]string
		mprime, ycruncher  int
		mprimeFailureValid bool
	}{
		{name: "resume with the same backends", reload: [][2]string{{mprimeOld, ycruncherOld}}, mprime: 10, ycruncher: 10},
		{name: "mprime updated", reload: [][2]string{{mprimeNew, ycruncherOld}}, mprime: 0, ycruncher: 10, mprimeFailureValid: true},
		{name: "y-cruncher updated", reload: [][2]string{{mprimeOld, ycruncherNew}}, mprime: 10, ycruncher: 0},
		{name: "both updated", reload: [][2]string{{mprimeNew, ycruncherNew}}, mprime: 0, ycruncher: 0, mprimeFailureValid: true},
		{name: "mprime rolled back", reload: [][2]string{{mprimeNew, ycruncherOld}, {mprimeOld, ycruncherOld}}, mprime: 10, ycruncher: 10},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -10}, coreStart{phase: journal.PhaseAtLimit, offset: -10})
			loadBackends(h, mprimeOld, ycruncherOld)
			liveTrials(h, mprime, profile, 1, failed)
			for _, w := range []machine.Workload{mprime, ycruncher} {
				liveTrials(h, w, profile, h.s.n, passed)
				for range h.s.n {
					h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "old", Seq: len(h.events) + 1, Evidence: EvidenceEpoch}, Class: journal.TrialClass{Regime: machine.R1, Workload: w.ID, Cores: []int{0}, DurationS: 90}, Condition: machine.Together, Profile: profile, Outcome: journal.OutcomePass, DurationS: 90})
				}
			}
			if h.s.fails(classOfWorkload(mprime), profile, 0) {
				t.Fatal("passes under the recording backend did not outweigh the earlier failure")
			}
			for _, paths := range tc.reload {
				loadBackends(h, paths[0], paths[1])
			}
			got := map[string]int{
				"mprime":     h.s.passes(classOfWorkload(mprime), profile, 0, allEvidence),
				"y-cruncher": h.s.passes(classOfWorkload(ycruncher), profile, 0, allEvidence),
			}
			want := map[string]int{"mprime": tc.mprime, "y-cruncher": tc.ycruncher}
			if diff := cmp.Diff(want, got); diff != "" {
				t.Fatalf("passes per backend after reload (-want +got):\n%s", diff)
			}
			if got := h.s.fails(classOfWorkload(mprime), profile, 0); got != tc.mprimeFailureValid {
				t.Fatalf("failure under the old mprime valid = %t, want %t", got, tc.mprimeFailureValid)
			}
			assertProjectionReplay(h)
		})
	}
}

func TestBackendUpdateRestartsCarriedSoloLimitEvidence(t *testing.T) {
	r1, r2 := machine.Workloads(machine.R1)[0], machine.Workloads(machine.R2)[0]
	if r1.Backend != machine.Mprime || r2.Backend != machine.Mprime {
		t.Fatalf("solo-limit workloads %s and %s no longer run mprime", r1.ID, r2.ID)
	}
	for _, tc := range []struct {
		name              string
		mprime, ycruncher string
		answered          bool
	}{
		{name: "same backends answer the check", mprime: mprimeOld, ycruncher: ycruncherOld, answered: true},
		{name: "other backend updated still answers", mprime: mprimeOld, ycruncher: ycruncherNew, answered: true},
		{name: "own backend updated needs live trials", mprime: mprimeNew, ycruncher: ycruncherOld},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t, coreStart{phase: journal.PhaseSearch, offset: -20})
			loadBackends(h, mprimeOld, ycruncherOld)
			for _, r := range []machine.Regime{machine.R1, machine.R2} {
				carryTrials(h, r, []int{0}, []int{-20}, h.s.durations.SearchTrialS, h.s.n, journal.OutcomePass)
			}
			loadBackends(h, tc.mprime, tc.ycruncher)
			h.add(&journal.TunerDecision{Core: 0, Phase: journal.PhaseSearch, Decision: journal.CheckSoloLimit, ToOffset: -20, Workloads: []string{r1.ID, r2.ID}})
			a, ok := h.s.perCore()
			if !ok {
				t.Fatal("no action for the candidate solo limit check")
			}
			if tc.answered {
				if p, phase := a.Payload.(*journal.CorePhase); !phase || p.To != journal.PhaseHasRoom {
					t.Fatalf("carried passes under the recorded backends did not answer the check: %+v", a)
				}
				return
			}
			if a.Kind != RunTrial || a.Trial.Regime != machine.R1 || a.Trial.Workload != r1.ID {
				t.Fatalf("check answered by passes from the previous mprime: %+v", a)
			}
		})
	}
}
