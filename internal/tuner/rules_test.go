package tuner

import (
	"encoding/json"
	"fmt"
	"math/rand/v2"
	"os"
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestConstraints(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -49, fail: new(-50)}, coreStart{phase: journal.PhaseAtLimit, offset: -49})
	h.add(&journal.Combination{Combination: 1, Hunt: 1, Members: []journal.CombinationMember{{Core: 0, Offset: -49}, {Core: 1, Offset: -50}}})
	tests := []struct {
		profile []int
		name    string
		reached bool
	}{{[]int{-49, -49}, "failure point -50 of core 00", false}, {[]int{-50, -49}, "failure point -50 of core 00", true}, {[]int{-49, -50}, "combination C1", true}}
	for _, tt := range tests {
		got, yes := h.s.Reaches(tt.profile)
		if yes != tt.reached || tt.reached && got != tt.name {
			t.Errorf("Reaches(%v) = %q,%v, want %q,%v", tt.profile, got, yes, tt.name, tt.reached)
		}
	}
	if i, ok := SoleNonzero([]int{0, -10, 0}); !ok || i != 1 {
		t.Errorf("SoleNonzero singleton = %d,%v", i, ok)
	}
	if _, ok := SoleNonzero([]int{-1, -1}); ok {
		t.Fatal("combination profile has a sole nonzero offset")
	}
	if _, ok := h.s.atLimit(h.s.core(0), []int{-49, -49}); !ok {
		t.Fatal("failure point does not put the core at its limit")
	}
}

func TestTogetherCrashNamesTheSoleNonzeroCoreByID(t *testing.T) {
	infos := []machine.CoreInfo{{Core: 0, CCD: 0, CPUs: []int{0, 16}}, {Core: 8, CCD: 1, CPUs: []int{8, 24}}}
	h := newHarnessOn(t, infos, config.Default(), coreStart{phase: journal.PhaseAtLimit}, coreStart{phase: journal.PhaseAtLimit, offset: -12})
	intent := h.add(&journal.TrialIntent{Trial: "0001", Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, DurationS: 120, Condition: machine.Together, Phase: journal.PhaseChecking, Cores: []int{0, 8}, Profile: []int{0, -12}})
	h.add(&journal.TrialEnd{Trial: "0001", Outcome: journal.OutcomeFailure, Signal: machine.Crash}, intent.Seq)
	a, ok := h.s.Attribution()
	if !ok {
		t.Fatal("no attribution for the failed together trial")
	}
	f := a.Payload.(*journal.Failure)
	if diff := cmp.Diff([]any{journal.Attributed, new(8), new(-12)}, []any{f.Attribution, f.Core, f.Offset}); diff != "" {
		t.Fatalf("attribution, core, offset (-want +got):\n%s", diff)
	}
}

func TestOptimum(t *testing.T) {
	tests := []struct {
		name                       string
		combinations               [][]int
		profile, hi, ranking, want []int
	}{
		{"two individually legal deepenings form a combination", [][]int{{0, 1}}, []int{-49, -49}, []int{0, 0}, []int{0, 1}, []int{-50, -49}},
		{"three-core backoff counterexample", [][]int{{0, 1}, {0, 2}}, []int{-49, -50, -50}, []int{-49, -49, -50}, []int{0, 1, 2}, []int{-49, -50, -50}},
		{"four-core cycle global", [][]int{{0, 1}, {1, 2}, {2, 3}, {3, 0}, {1, 3}}, []int{-49, -50, -49, -50}, []int{0, 0, 0, 0}, []int{0, 1, 2, 3}, []int{-50, -49, -50, -49}},
		{"better-ranked equal-sum later", [][]int{{0, 1}}, []int{-49, -49}, []int{0, 0}, []int{1, 0}, []int{-49, -50}},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			starts := make([]coreStart, len(tt.profile))
			for i, x := range tt.profile {
				starts[i] = coreStart{phase: journal.PhaseAtLimit, offset: x}
			}
			h := newHarness(t, starts...)
			for i, pair := range tt.combinations {
				h.add(&journal.Combination{Combination: i + 1, Hunt: i + 1, Members: []journal.CombinationMember{{Core: pair[0], Offset: -50}, {Core: pair[1], Offset: -50}}})
			}
			if got := h.s.optimum(tt.hi, tt.ranking); cmp.Diff(tt.want, got) != "" {
				t.Fatalf("optimum (-want +got):\n%s", cmp.Diff(tt.want, got))
			}
		})
	}
}

func TestOptimumWithStaircaseCombinations(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -20, fail: new(-27)}, coreStart{phase: journal.PhaseAtLimit, offset: -20, fail: new(-39)}, coreStart{phase: journal.PhaseAtLimit, offset: -20, fail: new(-30)})
	step := []int{-26, -38, -29}
	for k := range 39 {
		h.add(&journal.Combination{Combination: k + 1, Hunt: k + 1, Members: []journal.CombinationMember{{Core: 0, Offset: step[0]}, {Core: 1, Offset: step[1]}, {Core: 2, Offset: step[2]}}})
		step[2-k%3]++
	}
	var want []int
	for a := -26; a <= 0; a++ {
		for b := -38; b <= 0; b++ {
			for c := -29; c <= 0; c++ {
				p := []int{a, b, c}
				if _, reached := h.s.reaches(p); reached || want != nil && (totalDepth(p) > totalDepth(want) || totalDepth(p) == totalDepth(want) && slices.Compare(p, want) >= 0) {
					continue
				}
				want = p
			}
		}
	}
	if diff := cmp.Diff(want, h.s.optimum([]int{0, 0, 0}, []int{0, 1, 2})); diff != "" {
		t.Fatalf("optimum against exhaustive search (-want +got):\n%s", diff)
	}
}

func TestOptimumMatchesExhaustiveSearch(t *testing.T) {
	for seed := uint64(1); seed <= 400; seed++ {
		rng := rand.New(rand.NewPCG(seed, 0))
		n := 2 + rng.IntN(4)
		starts := make([]coreStart, n)
		for i := range starts {
			starts[i] = coreStart{phase: journal.PhaseAtLimit, offset: -30}
			if rng.IntN(2) == 0 {
				starts[i].fail = new(-50 + rng.IntN(4))
			}
		}
		h := newHarness(t, starts...)
		for k := range 1 + rng.IntN(40) {
			var members []journal.CombinationMember
			for _, i := range rng.Perm(n)[:2+rng.IntN(n-1)] {
				members = append(members, journal.CombinationMember{Core: i, Offset: -50 + rng.IntN(6)})
			}
			h.add(&journal.Combination{Combination: k + 1, Hunt: k + 1, Fallback: len(members) == n, Members: members})
		}
		hi := make([]int, n)
		for i := range hi {
			hi[i] = -47 + rng.IntN(6)
		}
		ranking := rng.Perm(n)
		if rng.IntN(4) == 0 {
			ranking = nil
		}
		if diff := cmp.Diff(exhaustiveOptimum(h.s, hi, ranking), h.s.optimum(hi, ranking)); diff != "" {
			t.Fatalf("seed %d: optimum against exhaustive search, ranking %v, hi %v (-want +got):\n%s", seed, ranking, hi, diff)
		}
	}
}

// The fixture is a combination backoff from a simulated session whose 50 accumulated
// fallback and CCD combinations once took the search over a minute.
func TestOptimumWithAccumulatedCombinations(t *testing.T) {
	data, err := os.ReadFile("testdata/optimum-accumulated-combinations.json")
	if err != nil {
		t.Fatal(err)
	}
	var fixture struct {
		FailurePoints     [][2]int `json:"failure_points"`
		Hi, Ranking, Want []int
		Combinations      [][][2]int
	}
	if err := json.Unmarshal(data, &fixture); err != nil {
		t.Fatal(err)
	}
	starts := make([]coreStart, len(fixture.Hi))
	for i := range starts {
		starts[i] = coreStart{phase: journal.PhaseAtLimit, offset: fixture.Hi[i]}
	}
	for _, f := range fixture.FailurePoints {
		starts[f[0]].fail = new(f[1])
	}
	h := newHarness(t, starts...)
	for k, c := range fixture.Combinations {
		members := make([]journal.CombinationMember, len(c))
		for i, m := range c {
			members[i] = journal.CombinationMember{Core: m[0], Offset: m[1]}
		}
		h.add(&journal.Combination{Combination: k + 1, Hunt: k + 1, Fallback: len(c) == len(starts), Members: members})
	}
	if diff := cmp.Diff(fixture.Want, h.s.optimum(fixture.Hi, fixture.Ranking)); diff != "" {
		t.Fatalf("optimum (-want +got):\n%s", diff)
	}
}

func exhaustiveOptimum(s *State, hi, ranking []int) []int {
	if len(ranking) != len(hi) {
		ranking = s.ids()
	}
	var best []int
	p := slices.Repeat([]int{machine.MinOffset}, len(hi))
	for {
		if _, reached := s.reaches(p); !reached && (best == nil || totalDepth(p) < totalDepth(best) || totalDepth(p) == totalDepth(best) && rankedDeeper(p, best, ranking)) {
			best = slices.Clone(p)
		}
		i := 0
		for i < len(p) && p[i] == hi[i] {
			p[i] = machine.MinOffset
			i++
		}
		if i == len(p) {
			return best
		}
		p[i]++
	}
}

func rankedDeeper(p, q, ranking []int) bool {
	for _, id := range ranking {
		if p[id] != q[id] {
			return p[id] < q[id]
		}
	}
	return false
}

func TestFourCoreCycleNeedsGlobalYield(t *testing.T) {
	h := newHarness(t,
		coreStart{phase: journal.PhaseAtLimit, offset: -49},
		coreStart{phase: journal.PhaseAtLimit, offset: -50},
		coreStart{phase: journal.PhaseAtLimit, offset: -49},
		coreStart{phase: journal.PhaseAtLimit, offset: -50},
	)
	for i, pair := range [][2]int{{0, 1}, {1, 2}, {2, 3}, {3, 0}, {1, 3}} {
		h.add(&journal.Combination{Combination: i + 1, Members: []journal.CombinationMember{{Core: pair[0], Offset: -50}, {Core: pair[1], Offset: -50}}})
	}
	p := h.s.offsets()
	global := h.s.optimum([]int{0, 0, 0, 0}, h.s.ids())
	if got := totalDepth(global); got != -198 {
		t.Fatalf("global optimum %v totals %d, want -198", global, got)
	}
	for _, breaker := range []int{1, 3} {
		bounded := append([]int(nil), p...)
		bounded[breaker] = -49
		target := h.s.optimum(bounded, h.s.ids())
		if got := totalDepth(target); got != -197 {
			t.Fatalf("backoff of core %d yields %v totaling %d, want -197", breaker, target, got)
		}
	}
}

func TestClassifyCrash(t *testing.T) {
	tests := []struct {
		name  string
		facts CrashFacts
		want  CrashKind
	}{
		{"trial", CrashFacts{InTrial: true}, CrashInTrial}, {"idle", CrashFacts{Applied: true}, CrashIdle}, {"stray", CrashFacts{}, CrashStray},
		{"evidence before thermal", CrashFacts{Applied: true, Evidence: true, Reason: machine.ResetReason{Kind: machine.ResetThermalTrip}}, CrashIdle},
		{"thermal", CrashFacts{InTrial: true, Reason: machine.ResetReason{Kind: machine.ResetThermalTrip}}, CrashThermal},
		{"power button trial", CrashFacts{InTrial: true, Reason: machine.ResetReason{Kind: machine.ResetPowerButton}}, CrashInTrial},
		{"power button idle", CrashFacts{Applied: true, Reason: machine.ResetReason{Kind: machine.ResetPowerButton}}, CrashInconclusive},
		{"confirmed absent reason", CrashFacts{Applied: true, Reason: machine.ResetReason{Supported: true}, Confirmed: true}, CrashInconclusive},
		{"unsupported absent reason", CrashFacts{Applied: true, Confirmed: true}, CrashIdle},
		{"unconfirmed absent reason", CrashFacts{Applied: true, Reason: machine.ResetReason{Supported: true}}, CrashIdle},
		{"unknown", CrashFacts{Reason: machine.ResetReason{Kind: machine.ResetUnknown}}, CrashStray},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := ClassifyCrash(tt.facts); got != tt.want {
				t.Fatalf("ClassifyCrash(%+v) = %d, want %d", tt.facts, got, tt.want)
			}
			recorded := &journal.CrashDetected{
				Stray:        tt.want == CrashStray,
				Inconclusive: tt.want == CrashInconclusive || tt.want == CrashThermal,
				ResetReason:  tt.facts.Reason.Kind,
			}
			if got := RecordedCrashKind(recorded, tt.facts.InTrial); got != tt.want {
				t.Fatalf("RecordedCrashKind(%+v, %v) = %d, want %d", recorded, tt.facts.InTrial, got, tt.want)
			}
		})
	}
}

// TestDirectFailureAtZero dead-ends a failure at CO 0 at once when its profile was all at 0, and otherwise once its
// rerun with every core at 0 failed too, citing both failures.
func TestDirectFailureAtZero(t *testing.T) {
	for _, tc := range []struct {
		name      string
		condition machine.Condition
		profile   []int
		core      *int
		idle      bool
	}{
		{"alone search", machine.Alone, []int{0, 0}, new(0), false},
		{"attributed together", machine.Together, []int{0, -10}, new(0), false},
		{"attributed failure at parked offsets", machine.Parked, []int{0, -10}, new(0), false},
		{"unattributed together", machine.Together, []int{0, 0}, nil, false},
		{"unattributed idle", machine.Together, []int{0, 0}, nil, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := hasRoomHarness(t, tc.profile...)
			if tc.condition == machine.Alone {
				h.add(&journal.CorePhase{Core: 0, To: journal.PhaseSearch, Offset: 0})
			}
			if tc.condition == machine.Parked {
				h.add(&journal.CorePhase{Core: 0, To: journal.PhaseAtLimit, Offset: -10})
				h.add(&journal.ProfileChange{From: tc.profile, To: []int{-10, -10}})
				h.add(&journal.HuntStart{Hunt: 1, Failing: []int{-10, -10}, Parked: []int{0, 0}, Candidates: []int{0, 1}, Trials: 5, Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Cores: []int{0, 1}, DurationS: 120})
			}
			var failure journal.Event
			if tc.idle {
				failure = h.add(&journal.Failure{Attribution: journal.Unattributed, Signal: machine.Crash, Condition: machine.Together, Profile: tc.profile})
			} else {
				tr := Trial{Core: 0, Offset: 0, Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Phase: journal.PhaseChecking, Condition: tc.condition, DurationS: 120, Profile: tc.profile, Cores: []int{0, 1}}
				if tc.condition == machine.Alone {
					tr.Cores, tr.Regime, tr.Phase = nil, machine.R1, journal.PhaseSearch
					tr.Workload = machine.Workloads(machine.R1)[0].ID
				}
				if tc.condition == machine.Parked {
					tr.Hunt, tr.Group = 1, 1
				}
				h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash, Core: tc.core})
				failure = h.decide(h.next())
				p, ok := failure.Data.(*journal.Failure)
				if !ok || cmp.Diff(tc.core, p.Core) != "" {
					t.Fatalf("zero attribution: %+v", failure)
				}
			}
			a := h.next()
			cause := []int{failure.Seq}
			if !allZero(tc.profile) {
				want := Trial{Regime: machine.R6, Workload: machine.Workloads(machine.R6)[0].ID, Cores: []int{0, 1}, DurationS: 120, Condition: machine.Parked, Phase: journal.PhaseChecking, Profile: []int{0, 0}, Rerun: true}
				if tc.condition == machine.Parked {
					want.Phase = journal.PhaseHunt
				}
				if diff := cmp.Diff(Action{Kind: RunTrial, Trial: want, Cause: []int{failure.Seq}}, a); diff != "" {
					t.Fatalf("all-zero rerun (-want +got):\n%s", diff)
				}
				_, rerun := h.trial(a, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash})
				h.decide(h.next())
				cause = append(cause, rerun.Seq)
				a = h.next()
			}
			dead, ok := a.Payload.(*journal.DeadEnd)
			if !ok || dead.Condition != journal.DeadEndFailureAtZero || cmp.Diff(tc.core, dead.Core) != "" || !slices.Equal(a.Cause, cause) {
				t.Fatalf("failure-at-zero transition: %+v", a)
			}
			before := h.s.Profile()
			offsets := h.s.offsets()
			h.decide(a)
			assertProjectionReplay(h)
			for range 3 {
				a = h.next()
				dead, ok = a.Payload.(*journal.DeadEnd)
				if !ok || dead.Condition != journal.DeadEndFailureAtZero || !slices.Equal(before, h.s.Profile()) || !slices.Equal(offsets, h.s.offsets()) {
					t.Fatalf("zero failure allowed tuning: %+v", a)
				}
			}
		})
	}
}

func TestThermalReasonYieldsToMCEEvidence(t *testing.T) {
	h := newHarness(t, searchAt(-10)...)
	intent := h.start(h.next())
	mce := h.add(&journal.MCE{Core: 0, BankType: machine.LoadStore})
	h.add(&journal.CrashDetected{InFlight: new(intent.Seq), ResetReason: machine.ResetThermalTrip}, mce.Seq)
	h.add(&journal.TrialEnd{Trial: intent.Data.(*journal.TrialIntent).Trial, Outcome: journal.OutcomeFailure, Signal: machine.Crash}, intent.Seq, mce.Seq)
	a := h.next()
	if p, ok := a.Payload.(*journal.Failure); !ok || p.Core == nil || *p.Core != 0 {
		t.Fatalf("failure attribution lost: %+v", a)
	}
}

func TestOptimumKeepsFailurePointOutsideSafeBounds(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -10, fail: new(-11)})
	for _, tc := range []struct {
		hi   int
		want []int
	}{{-10, []int{-10}}, {0, []int{-10}}} {
		if diff := cmp.Diff(tc.want, h.s.optimum([]int{tc.hi}, []int{0})); diff != "" {
			t.Fatalf("upper bound %d (-want +got):\n%s", tc.hi, diff)
		}
	}
}

func TestCarriedZeroFailurePointStopsBeforeTrial(t *testing.T) {
	h := newHarness(t, searchAt(0)...)
	phase := h.add(&journal.CorePhase{Core: 0, To: journal.PhaseSearch, Offset: 0, FailurePoint: new(0)})
	a := h.next()
	p, ok := a.Payload.(*journal.DeadEnd)
	if !ok || p.Condition != journal.DeadEndFailureAtZero || p.Core == nil || *p.Core != 0 || cmp.Diff([]int{phase.Seq}, a.Cause) != "" {
		t.Fatalf("zero failure point did not stop tuning: %+v", a)
	}
}

func TestClassTargetsPreserveHuntAndRerunLookup(t *testing.T) {
	for _, tt := range []struct {
		name    string
		offsets []int
		loaded  []int
		hunt    []int
		rerun   []int
		core    int
		offset  int
		idle    bool
	}{
		{"singleton before part in hunt", []int{-10}, nil, []int{0}, []int{0}, 0, 0, true},
		{"single core", []int{-10, -11, -12, -13}, []int{1}, []int{1}, nil, 1, -11, false},
		{"CCD part", []int{-10, -11, -12, -13}, []int{0, 1}, []int{0, 1}, []int{0, 1}, 0, 0, false},
		{"all cores", []int{-10, -11, -12, -13}, []int{0, 1, 2, 3}, []int{0, 1, 2, 3}, []int{0, 1, 2, 3}, 0, 0, false},
		{"unknown target", []int{-10, -11, -12, -13}, []int{0, 2}, []int{0, 2}, []int{0, 2}, 0, 0, false},
		{"empty target", []int{-10, -11, -12, -13}, nil, []int{0, 1, 2, 3}, nil, 0, 0, false},
	} {
		failedHarness := func(t *testing.T) (*harness, journal.Event) {
			t.Helper()
			h := hasRoomHarness(t, tt.offsets...)
			if tt.idle {
				// An idle failure's class is the all-core key, which topology resolves to the part before any trial evidence does.
				return h, h.add(&journal.Failure{Attribution: journal.Unattributed, Signal: machine.Crash, Condition: machine.Together, Regime: machine.R6, Profile: tt.offsets})
			}
			workload := machine.Workloads(machine.R6)[0].ID
			if tt.loaded == nil {
				// A trial that loads no core cannot be completed by the runner, so its events are written out.
				h.trials++
				id := fmt.Sprintf("%04d", h.trials)
				intent := h.add(&journal.TrialIntent{Trial: id, Regime: machine.R6, Workload: workload, DurationS: 120, Condition: machine.Together, Phase: journal.PhaseChecking, Profile: tt.offsets})
				h.add(&journal.TrialEnd{Trial: id, Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 5}, intent.Seq)
			} else {
				tr := Trial{Regime: machine.R6, Cores: tt.loaded, Workload: workload, Condition: machine.Together, Phase: journal.PhaseChecking, DurationS: 120, Profile: tt.offsets}
				h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 5})
			}
			attribution := h.next()
			if _, ok := attribution.Payload.(*journal.Failure); !ok {
				t.Fatalf("source attribution %+v", attribution)
			}
			return h, h.decide(attribution)
		}
		t.Run(tt.name, func(t *testing.T) {
			h, _ := failedHarness(t)
			start := driveToHuntStart(h).Payload.(*journal.HuntStart)
			if diff := cmp.Diff(tt.hunt, start.Cores, cmpopts.EquateEmpty()); diff != "" {
				t.Fatalf("hunt target (-want +got):\n%s", diff)
			}
		})
		t.Run(tt.name+"/rerun", func(t *testing.T) {
			h, failure := failedHarness(t)
			h.add(&journal.TunerDecision{Core: 0, Phase: journal.PhaseChecking, Decision: journal.Backoff, FromOffset: tt.offsets[0], ToOffset: tt.offsets[0] + 1, FailurePoint: new(tt.offsets[0])}, failure.Seq)
			var tr Trial
			for range 50 {
				a := h.next()
				if a.Kind == RunTrial && a.Trial.Rerun {
					tr = a.Trial
					break
				}
				if a.Kind != Decide {
					t.Fatalf("expected the failed class's rerun, got %+v", a)
				}
				h.decide(a)
			}
			if !tr.Rerun {
				t.Fatal("no rerun reached within 50 actions")
			}
			if diff := cmp.Diff([]any{tt.rerun, tt.core, tt.offset}, []any{tr.Cores, tr.Core, tr.Offset}, cmpopts.EquateEmpty()); diff != "" {
				t.Fatalf("rerun target (-want +got):\n%s", diff)
			}
		})
	}
}
