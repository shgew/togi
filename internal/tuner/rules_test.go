package tuner

import (
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestMarks(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseDone, offset: -49, fail: new(-50)}, coreStart{phase: journal.PhaseDone, offset: -49})
	h.add(&journal.MarkJoint{Mark: 1, Hunt: 1, Members: []journal.JointMember{{Core: 0, Offset: -49}, {Core: 1, Offset: -50}}})
	tests := []struct {
		profile []int
		name    string
		marked  bool
	}{{[]int{-49, -49}, "failed mark -50 of core 00", false}, {[]int{-50, -49}, "failed mark -50 of core 00", true}, {[]int{-49, -50}, "joint mark J1", true}}
	for _, tt := range tests {
		got, yes := h.s.Reaches(tt.profile)
		if yes != tt.marked || tt.marked && got != tt.name {
			t.Errorf("Reaches(%v) = %q,%v, want %q,%v", tt.profile, got, yes, tt.name, tt.marked)
		}
	}
	if i, ok := SoleNonzero([]int{0, -10, 0}); !ok || i != 1 {
		t.Errorf("SoleNonzero singleton = %d,%v", i, ok)
	}
	if _, ok := SoleNonzero([]int{-1, -1}); ok {
		t.Fatal("joint profile has a sole nonzero offset")
	}
	if _, ok := h.s.done(h.s.core(0), []int{-49, -49}); !ok {
		t.Fatal("failed mark does not make core done")
	}
}

func TestResidentCrashNamesTheSoleNonzeroCoreByID(t *testing.T) {
	h := &harness{t: t, s: New()}
	infos := []machine.CoreInfo{{Core: 0, CCD: 0, CPUs: []int{0, 16}}, {Core: 8, CCD: 1, CPUs: []int{8, 24}}}
	begin := h.add(&journal.SessionStart{Schema: journal.Schema, Session: "s", Cores: infos})
	h.add(&journal.ConfigLoaded{Path: config.DefaultPath, Config: config.Default()})
	h.add(&journal.CorePhase{Core: 0, To: journal.PhaseDone, Reason: "test"}, begin.Seq)
	h.add(&journal.CorePhase{Core: 8, To: journal.PhaseDone, Offset: -12, Reason: "test"}, begin.Seq)
	intent := h.add(&journal.TrialIntent{Trial: "0001", Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, DurationS: 120, Condition: machine.Resident, Phase: journal.PhaseGuard, Cores: []int{0, 8}, Profile: []int{0, -12}})
	h.add(&journal.TrialEnd{Trial: "0001", Outcome: journal.OutcomeFailure, Signal: machine.Crash}, intent.Seq)
	a, ok := h.s.Attribution()
	if !ok {
		t.Fatal("no attribution for the failed resident trial")
	}
	f := a.Payload.(*journal.Failure)
	if diff := cmp.Diff([]any{journal.Attributed, new(8), new(-12)}, []any{f.Attribution, f.Core, f.Offset}); diff != "" {
		t.Fatalf("attribution, core, offset (-want +got):\n%s", diff)
	}
}

func TestOptimum(t *testing.T) {
	tests := []struct {
		name                       string
		marks                      [][]int
		profile, hi, ranking, want []int
	}{
		{"two individually legal deepenings form a joint", [][]int{{0, 1}}, []int{-49, -49}, []int{0, 0}, []int{0, 1}, []int{-50, -49}},
		{"three-core backoff counterexample", [][]int{{0, 1}, {0, 2}}, []int{-49, -50, -50}, []int{-49, -49, -50}, []int{0, 1, 2}, []int{-49, -50, -50}},
		{"four-core cycle global", [][]int{{0, 1}, {1, 2}, {2, 3}, {3, 0}, {1, 3}}, []int{-49, -50, -49, -50}, []int{0, 0, 0, 0}, []int{0, 1, 2, 3}, []int{-50, -49, -50, -49}},
		{"better-ranked equal-sum later", [][]int{{0, 1}}, []int{-49, -49}, []int{0, 0}, []int{1, 0}, []int{-49, -50}},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			starts := make([]coreStart, len(tt.profile))
			for i, x := range tt.profile {
				starts[i] = coreStart{phase: journal.PhaseDone, offset: x}
			}
			h := newHarness(t, starts...)
			for i, pair := range tt.marks {
				h.add(&journal.MarkJoint{Mark: i + 1, Hunt: i + 1, Members: []journal.JointMember{{Core: pair[0], Offset: -50}, {Core: pair[1], Offset: -50}}})
			}
			if got := h.s.optimum(tt.profile, tt.hi, tt.ranking); cmp.Diff(tt.want, got) != "" {
				t.Fatalf("optimum (-want +got):\n%s", cmp.Diff(tt.want, got))
			}
		})
	}
}

func TestFourCoreCycleNeedsGlobalYield(t *testing.T) {
	h := newHarness(t,
		coreStart{phase: journal.PhaseDone, offset: -49},
		coreStart{phase: journal.PhaseDone, offset: -50},
		coreStart{phase: journal.PhaseDone, offset: -49},
		coreStart{phase: journal.PhaseDone, offset: -50},
	)
	for i, pair := range [][2]int{{0, 1}, {1, 2}, {2, 3}, {3, 0}, {1, 3}} {
		h.add(&journal.MarkJoint{Mark: i + 1, Members: []journal.JointMember{{Core: pair[0], Offset: -50}, {Core: pair[1], Offset: -50}}})
	}
	p := h.s.offsets()
	global := h.s.optimum(p, []int{0, 0, 0, 0}, h.s.ids())
	if got := totalDepth(global); got != -198 {
		t.Fatalf("global optimum %v totals %d, want -198", global, got)
	}
	for _, breaker := range []int{1, 3} {
		bounded := append([]int(nil), p...)
		bounded[breaker] = -49
		target := h.s.optimum(bounded, bounded, h.s.ids())
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
		})
	}
}

func TestThermalDeadEndNeedsNoEvidence(t *testing.T) {
	h := newHarness(t, searchAt(-10)...)
	intent := h.start(h.next())
	h.add(&journal.TrialProgress{Trial: intent.Data.(*journal.TrialIntent).Trial, Signal: machine.ComputationError})
	h.add(&journal.CrashDetected{PreviousBoot: "b", InFlight: new(intent.Seq), ResetReason: machine.ResetThermalTrip, ResetReasonRaw: "internal CPU thermal limit was tripped"})
	if h.s.thermal != nil {
		t.Fatal("recorded computation error was overridden by thermal reason")
	}
	h2 := newHarness(t, searchAt(-10)...)
	h2.add(&journal.CrashDetected{PreviousBoot: "b", ResetReason: machine.ResetThermalTrip, ResetReasonRaw: "internal CPU thermal limit was tripped"})
	a := h2.next()
	p, ok := a.Payload.(*journal.DeadEnd)
	if !ok || p.Condition != journal.DeadEndThermalTrip {
		t.Fatalf("thermal dead end %+v", a)
	}
}
