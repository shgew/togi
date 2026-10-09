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

func liveTrials(h *harness, w machine.Workload, profile []int, count int, outcome journal.TrialEnd) {
	h.t.Helper()
	for range count {
		h.trials++
		id := fmt.Sprintf("%04d", h.trials)
		intent := h.add(&journal.TrialIntent{Trial: id, Regime: machine.R1, Workload: w.ID, Cores: []int{0}, DurationS: 90, Condition: machine.Together, Profile: profile})
		end := outcome
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
			if h.s.failingSeq(classOfWorkload(mprime), profile, 0) != 0 {
				t.Fatal("passes under the recording backend did not outweigh the earlier failure")
			}
			for _, paths := range tc.reload {
				loadBackends(h, paths[0], paths[1])
			}
			passesPerBackend := func(s *State) map[string]int {
				return map[string]int{
					"mprime":     s.passes(classOfWorkload(mprime), profile, 0, allEvidence),
					"y-cruncher": s.passes(classOfWorkload(ycruncher), profile, 0, allEvidence),
				}
			}
			want := map[string]int{"mprime": tc.mprime, "y-cruncher": tc.ycruncher}
			if diff := cmp.Diff(want, passesPerBackend(h.s)); diff != "" {
				t.Fatalf("passes per backend after reload (-want +got):\n%s", diff)
			}
			replayed := replayState(h.events)
			if diff := cmp.Diff(want, passesPerBackend(replayed)); diff != "" {
				t.Fatalf("replayed passes per backend (-want +got):\n%s", diff)
			}
			if got := h.s.failingSeq(classOfWorkload(mprime), profile, 0) != 0; got != tc.mprimeFailureValid {
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

func TestBackendUpdateRestartsUnfinishedSearchStep(t *testing.T) {
	for _, tc := range []struct {
		name    string
		reload  [][2]string
		rerunR1 bool
		// rollback is recorded after the replacement R1 pass.
		rollback [][2]string
	}{
		{name: "same backends keep the R1 pass", reload: [][2]string{{mprimeOld, ycruncherOld}}},
		{name: "other backend updated keeps the R1 pass", reload: [][2]string{{mprimeOld, ycruncherNew}}},
		{name: "own backend updated reruns R1", reload: [][2]string{{mprimeNew, ycruncherOld}}, rerunR1: true},
		{name: "own backend rolled back restores the R1 pass", reload: [][2]string{{mprimeNew, ycruncherOld}, {mprimeOld, ycruncherOld}}},
		{name: "rollback after the replacement R1 still needs R2", reload: [][2]string{{mprimeNew, ycruncherOld}}, rerunR1: true, rollback: [][2]string{{mprimeOld, ycruncherOld}}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t, searchAt(-10)...)
			loadBackends(h, mprimeOld, ycruncherOld)
			a := h.next()
			if a.Kind != RunTrial || a.Trial.Regime != machine.R1 {
				t.Fatalf("search step starts with %+v, want R1", a)
			}
			intent, r1 := h.trial(a, passed)
			if w, _ := machine.WorkloadByID(intent.Data.(*journal.TrialIntent).Workload); w.Backend != machine.Mprime {
				t.Fatalf("first search R1 workload %s no longer runs mprime", w.ID)
			}
			for _, paths := range tc.reload {
				loadBackends(h, paths[0], paths[1])
			}
			assertNextReplays := func() Action {
				t.Helper()
				replayed := replayState(h.events)
				a := h.next()
				if diff := cmp.Diff(a, replayed.Next()); diff != "" {
					t.Fatalf("replayed next action (-live +replayed):\n%s", diff)
				}
				return a
			}
			a = assertNextReplays()
			want := []int{r1.Seq}
			if tc.rerunR1 {
				if a.Kind != RunTrial || a.Trial.Regime != machine.R1 {
					t.Fatalf("step after the mprime update continues with %+v, want R1 again", a)
				}
				again, rerun := h.trial(a, passed)
				want = []int{rerun.Seq}
				if len(tc.rollback) > 0 {
					if w, _ := machine.WorkloadByID(again.Data.(*journal.TrialIntent).Workload); w.Backend != machine.Ycruncher {
						t.Fatalf("replacement search R1 workload %s no longer runs y-cruncher", w.ID)
					}
					for _, paths := range tc.rollback {
						loadBackends(h, paths[0], paths[1])
					}
					want = []int{r1.Seq}
				}
				a = assertNextReplays()
			}
			if a.Kind != RunTrial || a.Trial.Regime != machine.R2 {
				t.Fatalf("step continues with %+v, want R2", a)
			}
			_, r2 := h.trial(a, passed)
			a = h.next()
			if d, ok := a.Payload.(*journal.TunerDecision); a.Kind != Decide || !ok || d.Decision != journal.StepDeeper {
				t.Fatalf("step ends with %+v, want step_deeper", a)
			}
			if diff := cmp.Diff(append(want, r2.Seq), a.Cause); diff != "" {
				t.Fatalf("step_deeper cause (-want +got):\n%s", diff)
			}
			assertProjectionReplay(h)
		})
	}
}
