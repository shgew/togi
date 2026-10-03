package tuner

import (
	"fmt"
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func carryTrials(h *harness, r machine.Regime, cores, profile []int, duration, count int, outcome journal.Outcome) []int {
	h.t.Helper()
	var seqs []int
	for range count {
		e := h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "20261002T004254Z", Seq: len(h.events) + 1, Evidence: EvidenceEpoch}, Class: journal.TrialClass{Regime: r, Workload: machine.Workloads(r)[0].ID, Cores: cores, DurationS: duration}, Condition: machine.Alone, Profile: profile, DurationS: duration, Outcome: outcome})
		seqs = append(seqs, e.Seq)
	}
	return seqs
}

func TestCarriedEvidenceConsumers(t *testing.T) {
	for _, tc := range []struct {
		name string
		run  func(*testing.T)
	}{
		{"candidate solo limit", func(t *testing.T) {
			t.Helper()
			h := newHarness(t, coreStart{phase: journal.PhaseSearch, offset: -20})
			var facts []int
			for _, r := range []machine.Regime{machine.R1, machine.R2} {
				facts = append(facts, carryTrials(h, r, []int{0}, []int{-20}, h.s.durations.SearchTrialS, h.s.n, journal.OutcomePass)...)
			}
			boundary := h.add(&journal.TunerDecision{Core: 0, Phase: journal.PhaseSearch, Decision: journal.CheckSoloLimit, ToOffset: -20, Workloads: []string{machine.Workloads(machine.R1)[0].ID, machine.Workloads(machine.R2)[0].ID}})
			a, ok := h.s.perCore()
			p, phase := a.Payload.(*journal.CorePhase)
			if !ok || !phase || p.To != journal.PhaseHasRoom {
				t.Fatalf("carried solo limit requires a live start: %+v", a)
			}
			if diff := cmp.Diff(append([]int{boundary.Seq}, facts...), a.Cause); diff != "" {
				t.Fatalf("solo limit evidence (-want +got):\n%s", diff)
			}
			if !strings.Contains(p.Reason, "carried") || !strings.Contains(p.Reason, "20261002T004254Z") {
				t.Fatalf("missing carried provenance: %s", p.Reason)
			}
		}},
		{"ordinary search", func(t *testing.T) {
			t.Helper()
			h := newHarness(t, coreStart{phase: journal.PhaseSearch, offset: -20})
			for _, r := range []machine.Regime{machine.R1, machine.R2} {
				carryTrials(h, r, []int{0}, []int{-20}, h.s.durations.SearchTrialS, h.s.n, journal.OutcomePass)
			}
			a, ok := h.s.perCore()
			if !ok || a.Kind != RunTrial || a.Trial.Regime != machine.R1 {
				t.Fatalf("ordinary search skipped its live starts: %+v", a)
			}
		}},
		{"checking lap", func(t *testing.T) {
			t.Helper()
			h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -20})
			h.add(&journal.ProfileChange{To: []int{-20}})
			carryTrials(h, machine.R1, []int{0}, []int{-20}, h.s.durations.CheckingTrialS, h.s.n, journal.OutcomePass)
			h.add(&journal.CheckingLap{Lap: 1, Event: journal.LapStart, Steps: []machine.Regime{machine.R1}})
			a := h.s.lapNext()
			if a.Kind != RunTrial || a.Trial.Regime != machine.R1 || a.Trial.Condition != machine.Together {
				t.Fatalf("lap credited on carried passes: %+v", a)
			}
			if st := h.s.projectChecking(); st.StepsDone != 0 {
				t.Fatalf("projection counted carried passes: %+v", st)
			}
			h.trial(a, passed)
			a = h.s.lapNext()
			if p, ok := a.Payload.(*journal.CheckingLap); !ok || !p.Passed {
				t.Fatalf("live pass did not finish lap: %+v", a)
			}
		}},
		{"rerun", func(t *testing.T) {
			t.Helper()
			h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -20})
			h.add(&journal.ProfileChange{To: []int{-20}})
			short := carryTrials(h, machine.R1, []int{0}, []int{-20}, h.s.durations.StartS, h.s.n, journal.OutcomePass)
			long := carryTrials(h, machine.R1, []int{0}, []int{-20}, 900, 1, journal.OutcomePass)
			h.s.obligations = []rerun{{trialClass{machine.R1, machine.Workloads(machine.R1)[0].ID, "[0]", 900}, len(h.events) + 1}}
			if a, ok := h.s.rerunNext(); ok {
				t.Fatalf("covered rerun scheduled a trial: %+v", a)
			}
			a := h.s.afterReruns(Action{Kind: Decide, Payload: &journal.CheckingLap{Lap: 1, Event: journal.LapStart}})
			if diff := cmp.Diff(append(short, long...), a.Cause); diff != "" {
				t.Fatalf("completed rerun provenance (-want +got):\n%s", diff)
			}
			if !strings.Contains(a.Payload.Message(), "20261002T004254Z") {
				t.Fatalf("completed rerun source missing: %s", a.Payload.Message())
			}
		}},
		{"deepening", func(t *testing.T) {
			t.Helper()
			h := newHarness(t, coreStart{phase: journal.PhaseHasRoom, offset: -10})
			var facts []int
			for _, r := range []machine.Regime{machine.R1, machine.R2, machine.R7} {
				facts = append(facts, carryTrials(h, r, []int{0}, []int{-11}, h.s.durations.StartS, h.s.n, journal.OutcomePass)...)
			}
			boundary := h.add(&journal.DeepeningRound{Round: 1, Event: journal.LapStart, Profile: []int{-11}, Target: []int{-12}, Cores: []int{0}, Starts: h.s.n, StartS: h.s.durations.StartS})
			a := h.s.roundCheck()
			p, ok := a.Payload.(*journal.DeepeningRound)
			if !ok || !p.Passed {
				t.Fatalf("carried deepening scheduled a trial: %+v", a)
			}
			if diff := cmp.Diff(append([]int{boundary.Seq}, facts...), a.Cause); diff != "" {
				t.Fatalf("deepening evidence (-want +got):\n%s", diff)
			}
			for _, c := range h.s.projectRound().Checks {
				if c.Passes != h.s.n {
					t.Fatalf("projection excluded carried starts: %+v", c)
				}
			}
		}},
		{"hunt group", func(t *testing.T) {
			t.Helper()
			h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -10}, coreStart{phase: journal.PhaseAtLimit, offset: -10})
			facts := carryTrials(h, machine.R7, []int{0, 1}, []int{-10, 0}, h.s.durations.StartS, h.s.n, journal.OutcomePass)
			h.add(&journal.HuntStart{Hunt: 1, Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, DurationS: h.s.durations.StartS, StartS: h.s.durations.StartS, Starts: h.s.n, Failing: []int{-10, -10}, Parked: []int{0, 0}, Candidates: []int{0, 1}})
			a := h.s.planGroup(h.s.hunt, groupPlan{cores: []int{0}, set: []int{0, 1}, stage: "probe", duration: h.s.durations.StartS}, "probe")
			p := a.Payload.(*journal.HuntGroup)
			if p.Inferred != "pass" {
				t.Fatalf("carried group needs a live start: %+v", p)
			}
			for _, seq := range facts {
				if !slices.Contains(a.Cause, seq) {
					t.Fatalf("group omitted carried cause #%d: %v", seq, a.Cause)
				}
			}
			h.decide(a)
			st := h.s.projectHunt().Groups[0]
			if st.Passes != h.s.n || st.Outcome != "pass" {
				t.Fatalf("projection omitted carried group: %+v", st)
			}
		}},
	} {
		t.Run(tc.name, tc.run)
	}
}

func TestCarriedFailureInvalidation(t *testing.T) {
	for _, tc := range []struct {
		name string
		idle bool
	}{
		{"trial", false},
		{"idle wildcard", true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -20})
			r := machine.R1
			if tc.idle {
				r = machine.R6
			}
			d := h.s.durations.StartS
			w := machine.Workloads(r)[0].ID
			k := trialClass{r, w, "[0]", d}
			carryTrials(h, r, []int{0}, []int{-20}, d, h.s.n, journal.OutcomePass)
			var failure journal.Event
			if tc.idle {
				failure = h.add(&journal.FailureCarried{Source: journal.FactSource{Session: "old"}, Signal: machine.Crash, Condition: machine.Together, Profile: []int{-19}})
			} else {
				seqs := carryTrials(h, r, []int{0}, []int{-19}, d, 1, journal.OutcomeFailure)
				failure = h.events[seqs[0]-1]
			}
			boundary := h.add(&journal.CheckingLap{Lap: 1, Event: journal.LapStart, Steps: []machine.Regime{r}}).Seq
			if got := h.s.passes(k, []int{-20}, boundary, deepeningEvidence); got != 0 {
				t.Fatalf("earlier passes survived failure: %d", got)
			}
			if got := h.s.failingSeq(k, []int{-20}, boundary); got != failure.Seq {
				t.Fatalf("later-scheduled class failure #%d, want #%d", got, failure.Seq)
			}
			for i := range h.s.n {
				tr := Trial{Regime: r, Workload: w, Core: 0, Cores: []int{0}, Profile: []int{-20}, DurationS: d, Condition: machine.Together}
				h.trial(Action{Kind: RunTrial, Trial: tr}, passed)
				if got := h.s.fails(k, []int{-20}, boundary); got != (i+1 < h.s.n) {
					t.Fatalf("class failing after %d live starts = %t", i+1, got)
				}
			}
			if got := h.s.passes(k, []int{-20}, boundary, deepeningEvidence); got != h.s.n {
				t.Fatalf("new live passes = %d, want %d", got, h.s.n)
			}
		})
	}
}

func TestResetClearsCarriedCoreEvidence(t *testing.T) {
	for _, outcome := range []journal.Outcome{journal.OutcomePass, journal.OutcomeFailure} {
		t.Run(string(outcome), func(t *testing.T) {
			h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -20}, coreStart{phase: journal.PhaseAtLimit, offset: -20})
			for _, id := range []int{0, 1} {
				p := []int{0, 0}
				p[id] = -20
				carryTrials(h, machine.R1, []int{id}, p, h.s.durations.StartS, h.s.n, outcome)
			}
			h.add(&journal.CommandReset{Core: new(0)})
			for _, id := range []int{0, 1} {
				k := trialClass{machine.R1, machine.Workloads(machine.R1)[0].ID, fmt.Sprint([]int{id}), h.s.durations.StartS}
				p := []int{0, 0}
				p[id] = -20
				got := h.s.passes(k, p, 0, soloLimitEvidence) > 0
				if outcome == journal.OutcomeFailure {
					got = h.s.fails(k, p, 0)
				}
				if got != (id == 1) {
					t.Fatalf("core %d evidence after resetting core 0 = %t", id, got)
				}
			}
			assertProjectionReplay(h)
		})
	}
}

func TestCarriedSoloLimitEligibility(t *testing.T) {
	for _, tc := range []struct {
		name     string
		profile  []int
		duration int
		workload int
		liveFail bool
	}{
		{"shallower profile", []int{-19}, 90, 0, false},
		{"different duration", []int{-20}, 120, 0, false},
		{"different frozen workload", []int{-20}, 90, 1, false},
		{"intervening live failure", []int{-20}, 90, 0, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := newHarness(t, coreStart{phase: journal.PhaseSearch, offset: -20})
			for _, r := range []machine.Regime{machine.R1, machine.R2} {
				for range h.s.n {
					h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "old"}, Class: journal.TrialClass{Regime: r, Workload: machine.Workloads(r)[tc.workload].ID, Cores: []int{0}, DurationS: tc.duration}, Profile: tc.profile, Condition: machine.Alone, Outcome: journal.OutcomePass, DurationS: tc.duration})
				}
			}
			if tc.liveFail {
				h.trial(Action{Kind: RunTrial, Trial: Trial{Core: 0, Offset: -20, Profile: []int{-20}, Condition: machine.Alone, Regime: machine.R1, Workload: machine.Workloads(machine.R1)[0].ID, DurationS: h.s.durations.SearchTrialS}}, failed)
			}
			h.add(&journal.TunerDecision{Core: 0, Phase: journal.PhaseSearch, Decision: journal.CheckSoloLimit, ToOffset: -20, Workloads: []string{machine.Workloads(machine.R1)[0].ID, machine.Workloads(machine.R2)[0].ID}})
			a, ok := h.s.perCore()
			if !ok || a.Kind != RunTrial || a.Trial.Regime != machine.R1 {
				t.Fatalf("ineligible carried solo limit was accepted: %+v", a)
			}
		})
	}
}

func TestCarriedFailureEstablishesLaterHuntGroup(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -10}, coreStart{phase: journal.PhaseAtLimit, offset: -10})
	failure := carryTrials(h, machine.R7, []int{0, 1}, []int{-10, 0}, h.s.durations.StartS, 1, journal.OutcomeFailure)[0]
	start := h.add(&journal.HuntStart{Hunt: 1, Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, DurationS: h.s.durations.StartS, StartS: h.s.durations.StartS, Starts: h.s.n, Failing: []int{-10, -10}, Parked: []int{0, 0}, Candidates: []int{0, 1}})
	a := h.s.planGroup(h.s.hunt, groupPlan{cores: []int{0}, set: []int{0, 1}, stage: "probe", duration: h.s.durations.StartS}, "probe")
	p := a.Payload.(*journal.HuntGroup)
	if p.Inferred != "failure" {
		t.Fatalf("carried failure did not establish later group: %+v", p)
	}
	if diff := cmp.Diff([]int{start.Seq, failure}, a.Cause); diff != "" {
		t.Fatalf("failure provenance (-want +got):\n%s", diff)
	}
	if !strings.Contains(p.Message(), "20261002T004254Z") {
		t.Fatalf("failure source missing: %s", p.Message())
	}
	h.decide(a)
	if got := h.s.projectHunt().Groups[0].Outcome; got != "failure" {
		t.Fatalf("carried failure projection = %s", got)
	}
}

func TestCarriedPassMonotonicityWarning(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -20})
	facts := carryTrials(h, machine.R1, []int{0}, []int{-20}, h.s.durations.StartS, h.s.n, journal.OutcomePass)
	_, failure := h.trial(Action{Kind: RunTrial, Trial: Trial{Core: 0, Offset: -19, Profile: []int{-19}, Condition: machine.Together, Regime: machine.R1, Workload: machine.Workloads(machine.R1)[0].ID, DurationS: h.s.durations.StartS}}, failed)
	h.decide(h.next())
	a := h.s.Next()
	p, ok := a.Payload.(*journal.TunerWarning)
	if !ok {
		t.Fatalf("failure did not contradict carried starts: %+v", a)
	}
	if diff := cmp.Diff(append([]int{failure.Seq}, facts...), a.Cause); diff != "" {
		t.Fatalf("warning evidence (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff(facts, p.Passes); diff != "" {
		t.Fatalf("contradicted carried passes (-want +got):\n%s", diff)
	}
	if !strings.Contains(p.Detail, "20261002T004254Z") {
		t.Fatalf("warning source missing: %s", p.Detail)
	}
	drained, ok := h.s.Drain()
	if !ok {
		t.Fatal("shutdown drain omitted the pending monotonicity warning")
	}
	if diff := cmp.Diff(a, drained); diff != "" {
		t.Fatalf("shutdown warning differs from Next (-Next +Drain):\n%s", diff)
	}
	if diff := cmp.Diff(a, h.s.Next()); diff != "" {
		t.Fatalf("reading warning actions changed the pending warning (-first +again):\n%s", diff)
	}
}

func TestResetClearsCarriedIdleFailure(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -20}, coreStart{phase: journal.PhaseAtLimit, offset: -20})
	failure := h.add(&journal.FailureCarried{Source: journal.FactSource{Session: "old"}, Class: journal.TrialClass{Regime: machine.R6, Cores: []int{0, 1}}, Signal: machine.Crash, Condition: machine.Together, Profile: []int{-20, -20}})
	k := trialClass{machine.R6, machine.Workloads(machine.R6)[0].ID, "[0 1]", 900}
	if !h.s.fails(k, []int{-20, -20}, failure.Seq+1) {
		t.Fatal("carried idle failure did not block the later R6 class")
	}
	h.add(&journal.CommandReset{Core: new(0)})
	if h.s.fails(k, []int{-20, -20}, 0) || h.s.failureBySeq(failure.Seq) != nil {
		t.Fatal("reset retained the carried all-core idle failure")
	}
}

func TestCarriedPassesDoNotAddSessionExposure(t *testing.T) {
	for _, condition := range []machine.Condition{machine.Together, machine.Parked} {
		t.Run(string(condition), func(t *testing.T) {
			h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -10})
			r := machine.R1
			w := machine.Workloads(r)[0].ID
			d := h.s.durations.StartS
			h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "old"}, Class: journal.TrialClass{Regime: r, Workload: w, Cores: []int{0}, DurationS: d}, Condition: condition, Profile: []int{-10}, Outcome: journal.OutcomePass, DurationS: d})
			boundary := h.add(&journal.ProfileChange{To: []int{-10}}).Seq
			k := trialClass{r, w, "[0]", d}
			if got := h.s.passes(k, []int{-10}, boundary, deepeningEvidence); got != 1 {
				t.Fatalf("carried pass unavailable to deepening: %d", got)
			}
			if got := h.s.projectChecking().Exposure; len(got) != 0 {
				t.Fatalf("archived observation counted as session exposure: %+v", got)
			}
			h.trial(Action{Kind: RunTrial, Trial: Trial{Core: 0, Offset: -10, Regime: r, Workload: w, DurationS: d, Condition: condition, Profile: []int{-10}}}, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: d})
			want := []journal.ExposureRow{{Regime: r, Workload: w, Starts: 1}}
			if diff := cmp.Diff(want, h.s.projectChecking().Exposure); diff != "" {
				t.Fatalf("live-only exposure (-want +got):\n%s", diff)
			}
		})
	}
}

func TestCarriedFailureSurvivesNewerPreBoundaryLiveFailure(t *testing.T) {
	for _, idle := range []bool{false, true} {
		for _, stage := range []string{"full", "probe"} {
			t.Run(fmt.Sprintf("idle=%t/%s", idle, stage), func(t *testing.T) {
				h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -10}, coreStart{phase: journal.PhaseAtLimit, offset: -10})
				r := machine.R7
				if idle {
					r = machine.R6
				}
				w := machine.Workloads(r)[0].ID
				d := h.s.durations.StartS
				var carried int
				if idle {
					carried = h.add(&journal.FailureCarried{Source: journal.FactSource{Session: "old"}, Class: journal.TrialClass{Regime: machine.R6, Cores: []int{0, 1}}, Signal: machine.Crash, Attribution: journal.Unattributed, Condition: machine.Together, Profile: []int{-9, -4}}).Seq
				} else {
					carried = h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "old"}, Class: journal.TrialClass{Regime: r, Workload: w, Cores: []int{0, 1}, DurationS: d}, Condition: machine.Parked, Profile: []int{-9, -4}, Outcome: journal.OutcomeFailure, Signal: machine.ComputationError}).Seq
				}
				tr := Trial{Regime: r, Workload: w, Cores: []int{0, 1}, DurationS: d, Condition: machine.Parked, Profile: []int{-10, -5}}
				h.trial(Action{Kind: RunTrial, Trial: tr}, failed)
				h.decide(h.next())
				sourceTrial := tr
				sourceTrial.Profile = []int{-10, -10}
				h.trial(Action{Kind: RunTrial, Trial: sourceTrial}, failed)
				source := h.decide(h.next())
				start := h.add(&journal.HuntStart{Hunt: 1, Failure: source.Seq, Regime: r, Workload: w, Cores: []int{0, 1}, DurationS: d, StartS: d, Starts: h.s.n, Failing: sourceTrial.Profile, Parked: []int{0, -5}, Candidates: []int{0, 1}})
				cores := []int{0, 1}
				if stage == "probe" {
					cores = []int{0}
				}
				plan := groupPlan{cores: cores, set: []int{0, 1}, stage: stage, duration: d}
				a := h.s.planGroup(h.s.hunt, plan, "probe")
				p := a.Payload.(*journal.HuntGroup)
				if p.Inferred != "failure" {
					t.Fatalf("newer pre-boundary live failure hid carried failure: %+v", p)
				}
				if diff := cmp.Diff([]int{start.Seq, carried}, a.Cause); diff != "" {
					t.Fatalf("admitted failure provenance (-want +got):\n%s", diff)
				}
				tr.Profile = []int{-10, -10}
				k := trialClass{r, w, "[0 1]", d}
				for i := range h.s.n {
					h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomePass, DurationS: d})
					want := carried
					if i+1 == h.s.n {
						want = 0
					}
					if got := h.s.failingSeq(k, p.Profile, start.Seq); got != want {
						t.Fatalf("admitted failure after %d newer passes = #%d, want #%d", i+1, got, want)
					}
				}
				if got := h.s.planGroup(h.s.hunt, plan, "probe").Payload.(*journal.HuntGroup).Inferred; got != "pass" {
					t.Fatalf("n newer passes did not cover carried failure: %s", got)
				}
			})
		}
	}
}
