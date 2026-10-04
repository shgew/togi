package tuner

import (
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func chainHarness(t *testing.T) *harness {
	t.Helper()
	h := hasRoomHarness(t, -10, -20, -30, -40, -20, -20, -20, -20)
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{machine.R7}})
	h.decide(h.s.cycleNext())
	return h
}

func passChainPart(t *testing.T, h *harness, cores []int, voltages map[int]float64) int {
	t.Helper()
	last := 0
	for trial := range 4 {
		a := h.s.cycleNext()
		wantDuration := h.s.durations.ShortTrialS
		if trial == 3 {
			wantDuration = h.s.longS(cores)
		}
		if a.Kind != RunTrial || !slices.Equal(a.Trial.Cores, cores) || a.Trial.Step != 1 || a.Trial.DurationS != wantDuration {
			t.Fatalf("part %v trial %d: %+v", cores, trial, a)
		}
		end := passed
		end.VoltageRequestsV = voltages
		_, e := h.trial(a, end)
		last = e.Seq
	}
	return last
}

func TestR7ChainDerivesFromCompletedPredecessor(t *testing.T) {
	h := chainHarness(t)
	full := []int{0, 1, 2, 3}
	seq := passChainPart(t, h, full, map[int]float64{0: 1.1, 1: 1.11, 2: 1.2, 3: 1.09})
	a := h.s.cycleNext()
	chain, ok := a.Payload.(*journal.CheckingChain)
	if !ok || !slices.Equal(chain.Cores, []int{0, 1, 3}) || !slices.Equal(chain.SourceSeqs, []int{seq}) || !slices.Equal(chain.Groups[0], []int{2}) {
		t.Fatalf("full telemetry did not derive partial 1: %+v", a)
	}
	h.decide(a)
	if h.s.projectChecking().StepsDone != 0 {
		t.Fatal("unfinished partial supplied completed-step coverage")
	}
	seq = passChainPart(t, h, chain.Cores, map[int]float64{0: 1.12, 1: 1.25, 3: 1.1})
	a = h.s.cycleNext()
	chain = a.Payload.(*journal.CheckingChain)
	if !slices.Equal(chain.Cores, []int{0, 3}) || !slices.Equal(chain.SourceSeqs, []int{seq}) {
		t.Fatalf("partial telemetry did not derive partial 2: %+v", chain)
	}
	h.decide(a)
	passChainPart(t, h, chain.Cores, map[int]float64{0: 1.2, 3: 1.1})
	a = h.s.cycleNext()
	end := a.Payload.(*journal.CheckingChain)
	if len(end.Cores) != 0 || !strings.Contains(end.Message(), "fewer than two") {
		t.Fatalf("chain failed to stop at two loaded cores: %+v", end)
	}
	h.decide(a)
	passChainPart(t, h, []int{4, 5, 6, 7}, nil)
	a = h.s.cycleNext()
	end = a.Payload.(*journal.CheckingChain)
	if len(end.Cores) != 0 || !strings.Contains(end.Message(), "offset fallback") {
		t.Fatalf("tied fallback did not end chain: %+v", end)
	}
	h.decide(a)
	passChainPart(t, h, h.s.ids(), nil)
	if p, ok := h.s.cycleNext().Payload.(*journal.CheckingCycle); !ok || !p.Passed {
		t.Fatal("completed chains did not complete the cycle")
	}
	assertProjectionReplay(h)
}

func TestR7ChainRequiresAllPredecessorPasses(t *testing.T) {
	h := chainHarness(t)
	for range 3 {
		a := h.s.cycleNext()
		h.trial(a, passed)
	}
	a := h.s.cycleNext()
	if a.Kind != RunTrial || a.Trial.DurationS != h.s.longS([]int{0, 1, 2, 3}) {
		t.Fatalf("derived before long predecessor requirement: %+v", a)
	}
	h.trial(a, unsure)
	a = h.s.cycleNext()
	if a.Kind != RunTrial || !a.Trial.Retry {
		t.Fatalf("inconclusive predecessor did not retry: %+v", a)
	}
	h.trial(a, passed)
	if _, ok := h.s.cycleNext().Payload.(*journal.CheckingChain); !ok {
		t.Fatal("completed predecessor did not derive the next part")
	}
}

func TestR7ChainFreezesStartedPartsAndRederivesUnstartedParts(t *testing.T) {
	for _, started := range []bool{false, true} {
		t.Run(map[bool]string{false: "unstarted", true: "inconclusive trial"}[started], func(t *testing.T) {
			h := chainHarness(t)
			passChainPart(t, h, []int{0, 1, 2, 3}, nil)
			a := h.s.cycleNext()
			old := a.Payload.(*journal.CheckingChain)
			h.decide(a)
			if started {
				h.trial(h.s.cycleNext(), unsure)
			}
			p := slices.Clone(h.s.Profile())
			p[1] = -1
			h.add(&journal.ProfileChange{From: h.s.Profile(), To: p})
			replay := New()
			for _, e := range h.events {
				replay.Fold(e)
			}
			a = replay.cycleNext()
			if started {
				if a.Kind != RunTrial || !a.Trial.Retry || !slices.Equal(a.Trial.Cores, old.Cores) {
					t.Fatalf("started part changed across profile change/resume: %+v", a)
				}
			} else {
				next, ok := a.Payload.(*journal.CheckingChain)
				if !ok || !slices.Equal(next.Cores, []int{0, 2, 3}) || !slices.Equal(next.Profile, p) {
					t.Fatalf("unstarted part did not rederive on resume: %+v", a)
				}
			}
			assertProjectionReplay(h)
		})
	}
}

func TestR7ChainKeepsStartedDescendantOfSatisfiedParent(t *testing.T) {
	h := chainHarness(t)
	passChainPart(t, h, []int{0, 1, 2, 3}, map[int]float64{0: 1.1, 1: 1.11, 2: 1.2, 3: 1.09})
	a := h.s.cycleNext()
	parent := a.Payload.(*journal.CheckingChain)
	h.decide(a)
	// Cycle evidence can meet a parent's requirements without a trial of its
	// own, so its child is the first part of the chain to run.
	child := []int{0, 3}
	h.add(&journal.CheckingChain{Cycle: 1, Step: 1, CCD: parent.CCD, Workload: parent.Workload, Part: "partial 2", Groups: [][]int{{1}, {0}, {3}}, Cores: child, Profile: slices.Clone(h.s.Profile())})
	h.trial(Action{Kind: RunTrial, Trial: Trial{Regime: machine.R7, Workload: parent.Workload, Cores: child, DurationS: h.s.durations.ShortTrialS, Phase: journal.PhaseChecking, Condition: machine.Together, Cycle: 1, Step: 1}}, unsure)
	p := slices.Clone(h.s.Profile())
	p[2] = -1
	h.add(&journal.ProfileChange{From: h.s.Profile(), To: p})
	if next, ok := h.s.cycleNext().Payload.(*journal.CheckingChain); ok && next.Part == "partial 1" {
		t.Fatalf("rederiving the parent dropped its started child: %+v", next)
	}
	if !slices.ContainsFunc(h.s.r7StepParts(0), func(cores []int) bool { return slices.Equal(cores, child) }) {
		t.Fatalf("started child left the cycle requirements: %v", h.s.r7StepParts(0))
	}
	assertProjectionReplay(h)
}

func TestLegacyPartialPassesAreOrdinaryEvidence(t *testing.T) {
	for _, carried := range []bool{false, true} {
		t.Run(map[bool]string{false: "live", true: "carried"}[carried], func(t *testing.T) {
			h := chainHarness(t)
			cores := []int{1, 2, 3}
			w := machine.Workloads(machine.R7)[0].ID
			for range h.s.n {
				if carried {
					h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "old", Seq: len(h.events) + 1, Trial: "partial"}, Class: journal.TrialClass{Regime: machine.R7, Workload: w, Cores: cores, DurationS: 120}, Profile: h.s.Profile(), Condition: machine.Together, Phase: journal.PhaseChecking, RecordOnly: true, Outcome: journal.OutcomePass})
				} else {
					h.add(&journal.TrialIntent{Trial: "legacy partial", Regime: machine.R7, Workload: w, Cores: cores, DurationS: 120, Profile: h.s.Profile(), Condition: machine.Together, Phase: journal.PhaseChecking, RecordOnly: true})
					h.add(&journal.TrialEnd{Trial: "legacy partial", Outcome: journal.OutcomePass, DurationS: 120})
				}
			}
			k := trialClass{machine.R7, w, coresKey(cores), 120}
			for _, rule := range []evidenceRule{allEvidence, rerunEvidence, deepeningEvidence} {
				if got := h.s.passes(k, h.s.Profile(), 0, rule); got != h.s.n {
					t.Fatalf("rule %d admitted %d legacy partial passes, want %d", rule, got, h.s.n)
				}
			}
			want := h.s.n
			if carried {
				want = 0
			}
			if got := h.s.passes(k, h.s.Profile(), h.s.checking.startSeq, cycleEvidence); got != want {
				t.Fatalf("cycle counted %d partial passes, want %d", got, want)
			}
		})
	}
}

func TestR7FullCycleIncludesEveryPartial(t *testing.T) {
	h := hasRoomHarness(t, -10, -20, -30, -40, -10, -20, -30, -40)
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: h.s.steps})
	partialTrials := 0
	for range 1000 {
		a := h.s.cycleNext()
		if a.Kind == RunTrial {
			if a.Trial.Regime == machine.R7 && len(a.Trial.Cores) < 4 {
				partialTrials++
			}
			h.trial(a, passed)
			continue
		}
		if end, ok := a.Payload.(*journal.CheckingCycle); ok {
			if !end.Passed || !end.Full || partialTrials != 48 {
				t.Fatalf("full cycle omitted partial trials: end %+v, trials %d", end, partialTrials)
			}
			h.decide(a)
			assertProjectionReplay(h)
			return
		}
		h.decide(a)
	}
	t.Fatal("cycle did not finish")
}

func TestR7OneCCDRunsFullThenChainOnce(t *testing.T) {
	h := chainHarness(t)
	for id := range h.s.ccd {
		h.s.ccd[id] = 0
	}
	h.s.parts = [][]int{h.s.ids()}
	passChainPart(t, h, h.s.ids(), nil)
	a := h.s.cycleNext()
	chain := a.Payload.(*journal.CheckingChain)
	if diff := cmp.Diff([]int{1, 2, 3, 4, 5, 6, 7}, chain.Cores); diff != "" {
		t.Fatal(diff)
	}
	h.decide(a)
	for range 100 {
		a = h.s.cycleNext()
		if end, ok := a.Payload.(*journal.CheckingCycle); ok {
			if !end.Passed {
				t.Fatal("one-CCD chain did not pass")
			}
			return
		}
		if a.Kind == RunTrial {
			if len(a.Trial.Cores) == 8 {
				t.Fatal("full/all-core part ran twice")
			}
			h.trial(a, passed)
		} else {
			h.decide(a)
		}
	}
	t.Fatal("one-CCD chain did not finish")
}

func TestR7ChainIdlesRequestTiesTogether(t *testing.T) {
	h := chainHarness(t)
	passChainPart(t, h, []int{0, 1, 2, 3}, map[int]float64{0: 1.1, 1: 1.1995, 2: 1.2, 3: 1.09})
	chain := h.s.cycleNext().Payload.(*journal.CheckingChain)
	if !slices.Equal(chain.Groups[0], []int{1, 2}) || !slices.Equal(chain.Cores, []int{0, 3}) {
		t.Fatalf("request tie was not idled together: %+v", chain)
	}
}

func TestR7PartialFailureDoesNotCompleteItsRequirement(t *testing.T) {
	h := chainHarness(t)
	passChainPart(t, h, []int{0, 1, 2, 3}, nil)
	h.decide(h.s.cycleNext())
	a := h.s.cycleNext()
	end := failed
	end.Core = new(a.Trial.Cores[0])
	intent, _ := h.trial(a, end)
	k := classOf(intent.Data.(*journal.TrialIntent))
	if len(h.s.ledger[k]) != 1 || h.s.ledger[k][0].pass || h.s.awaiting == nil {
		t.Fatal("partial failure did not enter ordinary decision evidence")
	}
	next := h.s.cycleNext()
	if next.Kind != RunTrial || next.Trial.DurationS != a.Trial.DurationS || !slices.Equal(next.Trial.Cores, a.Trial.Cores) {
		t.Fatalf("failed partial advanced the chain: %+v", next)
	}
}

func TestRepeatedR7PartialClassesAddTrials(t *testing.T) {
	h := chainHarness(t)
	h.s.checking.steps = []machine.Regime{machine.R7, machine.R7, machine.R7, machine.R7}
	profile := h.s.Profile()
	for _, step := range []int{1, 4} {
		if step != 1 {
			h.add(&journal.CheckingStep{Cycle: 1, Step: step, Profile: profile})
		}
		h.add(&journal.CheckingChain{Cycle: 1, Step: step, CCD: 0, Workload: machine.Workloads(machine.R7)[0].ID, Part: "partial 1", Profile: profile, Cores: []int{1, 2, 3}})
	}
	found := 0
	for _, q := range h.s.requirements(3) {
		if !slices.Equal(q.cores, []int{1, 2, 3}) {
			continue
		}
		found++
		want := 6
		if q.class.duration != h.s.durations.ShortTrialS {
			want = 2
		}
		if q.count != want {
			t.Fatalf("repeated partial class needs %d trials, want %d", q.count, want)
		}
	}
	if found != 2 {
		t.Fatalf("partial requirements = %d, want short and long", found)
	}
}
