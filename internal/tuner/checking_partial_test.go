package tuner

import (
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func chainHarness(t *testing.T) *harness {
	t.Helper()
	return chainHarnessOn(t, topology(8), config.Default(), machine.R7)
}

func chainHarnessOn(t *testing.T, infos []machine.CoreInfo, cfg config.Config, steps ...machine.Regime) *harness {
	t.Helper()
	h := newHarnessOn(t, infos, cfg, hasRoomStarts(-10, -20, -30, -40, -20, -20, -20, -20)...)
	h.decide(h.next())
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: steps})
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

var (
	chainFullRequests    = map[int]float64{0: 1.19, 1: 1.11, 2: 1.2, 3: 1.09}
	chainPartialRequests = map[int]float64{0: 1.19, 1: 1.25, 3: 1.1}
)

func shallower(h *harness, core, counts int) {
	h.t.Helper()
	p := slices.Clone(h.s.Profile())
	p[core] += counts
	h.add(&journal.ProfileChange{From: slices.Clone(h.s.Profile()), To: p})
}

func chainWithPartial(t *testing.T) *harness {
	t.Helper()
	h := chainHarness(t)
	passChainPart(t, h, []int{0, 1, 2, 3}, chainFullRequests)
	h.decide(h.s.cycleNext())
	return h
}

func sameNext(t *testing.T, h *harness) Action {
	t.Helper()
	a := h.s.cycleNext()
	if diff := cmp.Diff(a, replayState(h.events).cycleNext()); diff != "" {
		t.Fatalf("resume differs from live (-live +replayed):\n%s", diff)
	}
	return a
}

func rederive(t *testing.T, h *harness, part string, cores []int, change string) {
	t.Helper()
	a := sameNext(t, h)
	chain, ok := a.Payload.(*journal.CheckingChain)
	if !ok || chain.Part != part || !slices.Equal(chain.Cores, cores) || !slices.Equal(chain.Profile, h.s.Profile()) || !strings.Contains(chain.Message(), "re-derived after a profile change, loaded set "+change) {
		t.Fatalf("want %s re-derived at the new profile with %v and a %s set: %+v / %+v", part, cores, change, a, a.Payload)
	}
	h.decide(a)
}

func TestR7ChainRederivesAfterProfileChange(t *testing.T) {
	const (
		unchanged = "unchanged"
		changed   = "changed"
	)
	t.Run("unstarted node", func(t *testing.T) {
		h := chainWithPartial(t)
		shallower(h, 0, 1)
		rederive(t, h, "partial 1", []int{0, 1, 3}, unchanged)
		if a := sameNext(t, h); a.Kind != RunTrial || !slices.Equal(a.Trial.Cores, []int{0, 1, 3}) {
			t.Fatalf("re-derived part did not run: %+v", a)
		}
		assertProjectionReplay(h)
	})
	t.Run("inconclusive trial, set unchanged", func(t *testing.T) {
		h := chainWithPartial(t)
		h.trial(h.s.cycleNext(), unsure)
		shallower(h, 0, 1)
		rederive(t, h, "partial 1", []int{0, 1, 3}, unchanged)
		a := sameNext(t, h)
		if a.Kind != RunTrial || a.Trial.Retry || !slices.Equal(a.Trial.Cores, []int{0, 1, 3}) || a.Trial.DurationS != h.s.durations.ShortTrialS {
			t.Fatalf("saved retry survived the re-derivation or the class changed: %+v", a)
		}
		assertProjectionReplay(h)
	})
	t.Run("inconclusive trial, set changed", func(t *testing.T) {
		h := chainWithPartial(t)
		h.trial(h.s.cycleNext(), unsure)
		shallower(h, 0, 10)
		rederive(t, h, "partial 1", []int{1, 2, 3}, changed)
		if a := sameNext(t, h); a.Kind != RunTrial || a.Trial.Retry || !slices.Equal(a.Trial.Cores, []int{1, 2, 3}) {
			t.Fatalf("changed set did not run its own trials: %+v", a)
		}
		assertProjectionReplay(h)
	})
	t.Run("passed node, set unchanged", func(t *testing.T) {
		h := chainWithPartial(t)
		passChainPart(t, h, []int{0, 1, 3}, chainPartialRequests)
		shallower(h, 0, 1)
		rederive(t, h, "partial 1", []int{0, 1, 3}, unchanged)
		a := sameNext(t, h)
		if next, ok := a.Payload.(*journal.CheckingChain); !ok || next.Part != "partial 2" || !slices.Equal(next.Cores, []int{0, 3}) {
			t.Fatalf("passed part with an unchanged set ran again or did not continue the chain: %+v", a)
		}
		assertProjectionReplay(h)
	})
	t.Run("passed node, set changed", func(t *testing.T) {
		h := chainWithPartial(t)
		passChainPart(t, h, []int{0, 1, 3}, chainPartialRequests)
		shallower(h, 0, 10)
		rederive(t, h, "partial 1", []int{1, 2, 3}, changed)
		passChainPart(t, h, []int{1, 2, 3}, nil)
		if next, ok := sameNext(t, h).Payload.(*journal.CheckingChain); !ok || next.Part != "partial 2" {
			t.Fatalf("changed set did not continue the chain after its passes: %+v", next)
		}
		assertProjectionReplay(h)
	})
	t.Run("satisfied parent and started child", func(t *testing.T) {
		h := chainWithPartial(t)
		parent := h.s.checking.partial[1].chains[0][0].start
		child := []int{0, 3}
		h.add(&journal.CheckingChain{Cycle: 1, Step: 1, CCD: parent.CCD, Workload: parent.Workload, Part: "partial 2", Groups: [][]int{{1}, {0}, {3}}, Cores: child, Profile: slices.Clone(h.s.Profile())})
		h.trial(Action{Kind: RunTrial, Trial: Trial{Regime: machine.R7, Workload: parent.Workload, Cores: child, DurationS: h.s.durations.ShortTrialS, Phase: journal.PhaseChecking, Condition: machine.Together, Cycle: 1, Step: 1}}, unsure)
		shallower(h, 2, 1)
		if slices.ContainsFunc(h.s.r7StepParts(0), func(cores []int) bool { return slices.Equal(cores, child) || slices.Equal(cores, parent.Cores) }) {
			t.Fatalf("stale parts stayed in the cycle requirements: %v", h.s.r7StepParts(0))
		}
		rederive(t, h, "partial 1", []int{0, 1, 3}, unchanged)
		if !slices.ContainsFunc(h.s.r7StepParts(0), func(cores []int) bool { return slices.Equal(cores, parent.Cores) }) {
			t.Fatalf("re-derived parent is not required: %v", h.s.r7StepParts(0))
		}
		if slices.ContainsFunc(h.s.r7StepParts(0), func(cores []int) bool { return slices.Equal(cores, child) }) {
			t.Fatalf("child stayed required before its own re-derivation: %v", h.s.r7StepParts(0))
		}
		assertProjectionReplay(h)
	})
	t.Run("change on the other CCD only", func(t *testing.T) {
		h := chainWithPartial(t)
		passChainPart(t, h, []int{0, 1, 3}, chainPartialRequests)
		h.decide(h.s.cycleNext())
		passChainPart(t, h, []int{0, 3}, map[int]float64{0: 1.2, 3: 1.1})
		h.decide(h.s.cycleNext())
		shallower(h, 4, 1)
		rederive(t, h, "partial 1", []int{0, 1, 3}, unchanged)
		rederive(t, h, "partial 2", []int{0, 3}, unchanged)
		rederive(t, h, "partial 3", nil, unchanged)
		if a := sameNext(t, h); a.Kind != RunTrial || slices.Contains(a.Trial.Cores, 0) {
			t.Fatalf("the unchanged CCD ran a new trial: %+v", a)
		}
		assertProjectionReplay(h)
	})
	t.Run("several backoffs before revisiting", func(t *testing.T) {
		h := chainWithPartial(t)
		before := len(h.events)
		for _, core := range []int{0, 1, 3} {
			shallower(h, core, 1)
		}
		rederive(t, h, "partial 1", []int{0, 1, 3}, unchanged)
		count := 0
		for _, e := range h.events[before:] {
			if _, ok := e.Data.(*journal.CheckingChain); ok {
				count++
			}
		}
		if count != 1 {
			t.Fatalf("%d chain records for three profile changes, want 1", count)
		}
		assertProjectionReplay(h)
	})
	t.Run("one CCD", func(t *testing.T) {
		h := chainHarnessOn(t, onOneCCD(topology(8)), config.Default(), machine.R7)
		passChainPart(t, h, h.s.ids(), nil)
		h.decide(h.s.cycleNext())
		shallower(h, 0, 1)
		rederive(t, h, "partial 1", []int{1, 2, 3, 4, 5, 6, 7}, unchanged)
		for range 100 {
			a := sameNext(t, h)
			if end, ok := a.Payload.(*journal.CheckingCycle); ok {
				if !end.Passed {
					t.Fatal("one-CCD chain did not pass")
				}
				assertProjectionReplay(h)
				return
			}
			if a.Kind == RunTrial {
				if len(a.Trial.Cores) == 8 {
					t.Fatalf("the full part re-ran: %+v", a)
				}
				h.trial(a, passed)
				continue
			}
			h.decide(a)
		}
		t.Fatal("one-CCD chain did not finish")
	})
	t.Run("repeated step of one workload", func(t *testing.T) {
		h := chainHarnessOn(t, topology(8), config.Default(), machine.R7, machine.R7, machine.R7, machine.R7)
		w := machine.Workloads(machine.R7)[0].ID
		h.add(&journal.CheckingStep{Cycle: 1, Step: 4, Profile: slices.Clone(h.s.Profile())})
		for _, step := range []int{1, 4} {
			h.add(&journal.CheckingChain{Cycle: 1, Step: step, CCD: 0, Workload: w, Part: "partial 1", Profile: slices.Clone(h.s.Profile()), Cores: []int{1, 2, 3}})
		}
		shallower(h, 0, 1)
		for _, step := range []int{1, 4} {
			h.add(&journal.CheckingChain{Cycle: 1, Step: step, CCD: 0, Workload: w, Part: "partial 1", Profile: slices.Clone(h.s.Profile()), Cores: []int{1, 2, 3}})
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
				t.Fatalf("repeated partial class needs %d trials after re-derivation, want %d", q.count, want)
			}
		}
		if found != 2 {
			t.Fatalf("partial requirements = %d, want short and long", found)
		}
	})
}

func TestR7ChainRerunsAFailedClassThatLeftTheChain(t *testing.T) {
	h := chainHarness(t)
	passChainPart(t, h, []int{0, 1, 2, 3}, map[int]float64{0: 1.1985, 1: 1.11, 2: 1.2, 3: 1.09})
	h.decide(h.s.cycleNext())
	old := []int{0, 1, 3}
	end := failed
	end.Core = new(0)
	h.trial(h.s.cycleNext(), end)
	var a Action
	for range 6 {
		if a = h.next(); a.Kind == RunTrial {
			break
		}
		h.decide(a)
	}
	if got := h.s.Profile()[0]; got != -9 {
		t.Fatalf("core 0 at %d, want one count back", got)
	}
	for range h.s.n {
		if a.Kind != RunTrial || !a.Trial.Rerun || !slices.Equal(a.Trial.Cores, old) {
			t.Fatalf("the failed class did not rerun before the cycle resumed: %+v", a)
		}
		h.trial(a, passed)
		a = h.next()
	}
	rederive(t, h, "partial 1", []int{1, 2, 3}, "changed")
	if slices.ContainsFunc(h.s.r7StepParts(0), func(cores []int) bool { return slices.Equal(cores, old) }) {
		t.Fatalf("a class that left the chain is still required: %v", h.s.r7StepParts(0))
	}
	assertProjectionReplay(h)
}

var reorderRequests = map[int]float64{0: 1.185, 1: 1.180, 2: 1.170, 3: 1.160, 4: 1.250, 5: 1.190, 6: 1.215, 7: 1.150}

func reorderMeasurement(h *harness, cores []int) map[int]float64 {
	out := map[int]float64{}
	for _, c := range cores {
		if c > 7 {
			return nil
		}
		out[c] = reorderRequests[c]
		if c == 0 {
			out[c] += float64(h.s.Profile()[0]+24) * 0.0036
		}
	}
	return out
}

func TestR7ChainRetestsReorderedPartAfterBackoff(t *testing.T) {
	offsets := make([]int, 16)
	for i := range offsets {
		offsets[i] = -24
		if i > 7 {
			offsets[i] = -20
		}
	}
	h := newHarnessOn(t, topology(16), config.Default(), hasRoomStarts(offsets...)...)
	h.decide(h.next())
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: []machine.Regime{machine.R7, machine.R1}})
	h.decide(h.s.cycleNext())

	riskiest := []int{1, 2, 3, 5, 7}
	isRiskiest := func(cores []int) bool { return slices.Equal(cores, riskiest) }
	step := func(a Action) {
		t.Helper()
		if a.Kind != RunTrial {
			h.decide(a)
			return
		}
		end := passed
		end.VoltageRequestsV = reorderMeasurement(h, a.Trial.Cores)
		h.trial(a, end)
	}

	for range 400 {
		a := h.next()
		if a.Kind == RunTrial && a.Trial.Regime == machine.R1 {
			break
		}
		step(a)
	}
	for _, e := range h.events {
		if p, ok := e.Data.(*journal.TrialIntent); ok && isRiskiest(p.Cores) {
			t.Fatalf("the riskiest load ran before the backoffs: %+v", p)
		}
	}
	firstBackoff := len(h.events)

	failures := 0
	var offsets0 []int
	finished := false
	for range 1500 {
		if len(h.events) > firstBackoff && failures > 0 {
			if diff := cmp.Diff(h.s.Next(), replayState(h.events).Next()); diff != "" {
				t.Fatalf("resume after event %d differs (-live +replayed):\n%s", len(h.events), diff)
			}
		}
		a := h.next()
		if end, ok := a.Payload.(*journal.CheckingCycle); ok {
			if !end.Passed {
				t.Fatalf("cycle ended without passing: %+v", end)
			}
			if slices.ContainsFunc(h.s.r7StepParts(0), func(cores []int) bool { return slices.Equal(cores, []int{0, 1, 2, 3, 7}) }) {
				t.Fatalf("the old-order part is still required: %v", h.s.r7StepParts(0))
			}
			h.decide(a)
			finished = true
			break
		}
		if a.Kind == RunTrial && a.Trial.Regime == machine.R1 && !a.Trial.Retry && a.Trial.Core == 0 && failures < 3 {
			end := failed
			end.Core = new(0)
			h.trial(a, end)
			failures++
			continue
		}
		step(a)
		if p, ok := h.events[len(h.events)-1].Data.(*journal.ProfileChange); ok {
			offsets0 = append(offsets0, p.To[0])
		}
	}
	if !finished {
		t.Fatal("cycle did not end")
	}
	if diff := cmp.Diff([]int{-23, -22, -21}, offsets0); diff != "" {
		t.Fatalf("core 0 backoffs (-want +got):\n%s", diff)
	}

	type derived struct {
		part   string
		cores  []int
		change string
	}
	got := map[int][]derived{}
	var trials [][]int
	var long, short, riskiestEnd int
	rerunsAfterBackoff := true
	for i, e := range h.events[firstBackoff:] {
		switch p := e.Data.(type) {
		case *journal.CheckingChain:
			change := ""
			switch {
			case strings.Contains(p.Msg, "loaded set unchanged"):
				change = "unchanged"
			case strings.Contains(p.Msg, "loaded set changed"):
				change = "changed"
			}
			if !slices.Equal(p.Profile, h.s.Profile()) {
				t.Fatalf("chain record at a stale profile: %+v", p)
			}
			got[p.CCD] = append(got[p.CCD], derived{p.Part, p.Cores, change})
		case *journal.TrialIntent:
			if p.Regime == machine.R7 {
				trials = append(trials, p.Cores)
				if isRiskiest(p.Cores) {
					if p.DurationS == h.s.durations.ShortTrialS {
						short++
					} else {
						long++
					}
					riskiestEnd = firstBackoff + i
				}
			}
		case *journal.ProfileChange:
			next := h.events[firstBackoff+i+1:]
			j := slices.IndexFunc(next, func(e journal.Event) bool { _, ok := e.Data.(*journal.TrialIntent); return ok })
			if j < 0 || !next[j].Data.(*journal.TrialIntent).Rerun {
				rerunsAfterBackoff = false
			}
		}
	}
	if !rerunsAfterBackoff {
		t.Fatal("a backoff was not followed by its failed class's rerun")
	}
	want0 := []derived{
		{"partial 1", []int{0, 1, 2, 3, 5, 6, 7}, "unchanged"},
		{"partial 2", []int{0, 1, 2, 3, 5, 7}, "unchanged"},
		{"partial 3", riskiest, "changed"},
		{"partial 4", []int{1, 2, 3, 7}, "unchanged"},
		{"partial 5", []int{2, 3, 7}, "unchanged"},
		{"partial 6", []int{3, 7}, "unchanged"},
		{"partial 7", nil, "unchanged"},
	}
	if diff := cmp.Diff(want0, got[0], cmp.AllowUnexported(derived{})); diff != "" {
		t.Fatalf("CCD 0 re-derivations (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff([]derived{{"partial 1", nil, "unchanged"}}, got[1], cmp.AllowUnexported(derived{})); diff != "" {
		t.Fatalf("CCD 1 re-derivations (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff([][]int{riskiest, riskiest, riskiest, riskiest}, trials); diff != "" {
		t.Fatalf("R7 trials after the backoffs (-want +got):\n%s", diff)
	}
	if short != 3 || long != 1 || riskiestEnd == 0 {
		t.Fatalf("riskiest load ran %d short and %d long trials", short, long)
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
	h := chainHarnessOn(t, onOneCCD(topology(8)), config.Default(), machine.R7)
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
	steps := []machine.Regime{machine.R7, machine.R7, machine.R7, machine.R7}
	h := chainHarnessOn(t, topology(8), config.Default(), steps...)
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
