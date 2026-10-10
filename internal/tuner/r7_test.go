package tuner

import (
	"fmt"
	"regexp"
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func r7Harness(t *testing.T) *harness {
	t.Helper()
	h := newHarness(t, coreStart{phase: journal.PhaseHasRoom, offset: -30}, coreStart{phase: journal.PhaseHasRoom, offset: -30}, coreStart{phase: journal.PhaseHasRoom, offset: -30}, coreStart{phase: journal.PhaseHasRoom, offset: -30})
	h.add(&journal.ProfileChange{To: []int{-30, -30, -30, -30}})
	h.add(&journal.HostRanking{Ranking: []int{0, 1, 2, 3}})
	return h
}
func r7Fact(h *harness, pass bool, cores, profile []int, requests map[int]float64, top []int, named, stalled *int, clocks map[int]int) journal.Event {
	outcome := journal.OutcomeFailure
	if pass {
		outcome = journal.OutcomePass
	}
	return h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "ruleset8", Seq: len(h.events) + 1, Trial: fmt.Sprint(len(h.events))}, Class: journal.TrialClass{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: cores, DurationS: 120}, RecordOnly: true, Condition: machine.Together, Profile: profile, Outcome: outcome, Signal: machine.ComputationError, Core: named, StalledCore: stalled, VoltageRequestsV: requests, TopRequesters: top, CCDMHz: clocks})
}
func TestR7FailuresMoveAfterLongCleanLedger(t *testing.T) {
	h := r7Harness(t)
	cores := []int{0, 1}
	for range 100 {
		r7Fact(h, true, cores, h.s.Profile(), map[int]float64{0: 1.1, 1: 1.08}, []int{0}, nil, nil, nil)
	}
	for _, want := range []int{-29, -28} {
		r7Fact(h, false, cores, h.s.offsets(), map[int]float64{0: 1.1, 1: 1.08}, []int{0}, nil, nil, nil)
		a, ok := h.s.Drain()
		if !ok {
			t.Fatal("failure did not move")
		}
		move, ok := a.Payload.(*journal.TunerDecision)
		if !ok || move.Decision != journal.Backoff {
			t.Fatalf("%+v", a)
		}
		if diff := cmp.Diff(want, move.ToOffset); diff != "" {
			t.Fatal(diff)
		}
		h.decide(a)
	}
	if len(h.s.queue) != 0 {
		t.Fatal("R7 queued a hunt")
	}
	scheduled := Action{Kind: RunTrial, Trial: Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: cores, DurationS: 120, Condition: machine.Together, Profile: h.s.Profile()}}
	if diff := cmp.Diff(RunTrial, h.s.skipKnownFailure(scheduled).Kind); diff != "" {
		t.Fatal(diff)
	}
	assertProjectionReplay(h)
}
func TestR7AttributionAndTieBackoff(t *testing.T) {
	for _, tc := range []struct {
		name           string
		cores, top     []int
		named, stalled *int
		want           []int
	}{
		{"named", []int{0, 1}, []int{0}, new(1), nil, []int{1}},
		{"named idle core", []int{0, 1}, []int{0}, new(3), nil, []int{3}},
		{"whole CCD", []int{0, 1}, []int{0}, nil, nil, []int{0}},
		{"all CCDs", []int{0, 1, 2, 3}, []int{0, 2}, nil, nil, []int{0, 2}},
		{"stalled CCD", []int{0, 1, 2, 3}, []int{0, 2}, nil, new(3), []int{2}},
		{"tied CCD", []int{0, 1}, []int{0, 1}, nil, nil, []int{1}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := r7Harness(t)
			r7Fact(h, false, tc.cores, h.s.Profile(), nil, tc.top, tc.named, tc.stalled, nil)
			var moved []int
			for range 8 {
				a, ok := h.s.Drain()
				if !ok {
					break
				}
				p, ok := a.Payload.(*journal.TunerDecision)
				if !ok {
					t.Fatalf("unexpected %+v", a)
				}
				if p.Decision == journal.Backoff {
					moved = append(moved, p.Core)
					if p.ToOffset != -29 {
						t.Fatalf("no-pass fallback=%d", p.ToOffset)
					}
				}
				h.decide(a)
			}
			if diff := cmp.Diff(tc.want, moved); diff != "" {
				t.Fatal(diff)
			}
			if len(h.s.queue) != 0 || h.s.hunt != nil {
				t.Fatal("multi-core R7 hunted")
			}
			assertProjectionReplay(h)
		})
	}
}
func TestR7VoltageTargetClockFilter(t *testing.T) {
	for _, tc := range []struct {
		name   string
		clocks map[int]int
		want   int
	}{{"equal clock", map[int]int{0: 5000}, -20}, {"lower clock", map[int]int{0: 4900}, -29}, {"missing pass clock", nil, -29}} {
		t.Run(tc.name, func(t *testing.T) {
			h := r7Harness(t)
			cores := []int{0, 1}
			for range h.s.n {
				r7Fact(h, true, cores, []int{-20, -30, -30, -30}, map[int]float64{0: 1.136, 1: 1.08}, []int{0}, nil, nil, tc.clocks)
			}
			r7Fact(h, false, cores, h.s.Profile(), map[int]float64{0: 1.1, 1: 1.08}, []int{0}, new(0), nil, map[int]int{0: 5000})
			a, ok := h.s.Drain()
			if !ok {
				t.Fatal("no backoff")
			}
			p, ok := a.Payload.(*journal.TunerDecision)
			if !ok || p.Decision != journal.Backoff {
				t.Fatalf("%+v", a)
			}
			if diff := cmp.Diff(tc.want, p.ToOffset); diff != "" {
				t.Fatal(diff)
			}
			if !strings.Contains(p.Reason, "voltage-targeted") {
				t.Fatal(p.Reason)
			}
		})
	}
}
func TestR7TransitionRecordOnlyFailuresMoveBeforeTrial(t *testing.T) {
	h := r7Harness(t)
	for range 4 {
		r7Fact(h, false, []int{0, 1}, h.s.Profile(), nil, []int{0}, new(1), nil, nil)
	}
	a := h.next()
	p, ok := a.Payload.(*journal.TunerDecision)
	if !ok || p.Decision != journal.Backoff || p.Core != 1 {
		t.Fatalf("first action %+v", a)
	}
}
func TestR7RequestOrderShiftsAndFallsBack(t *testing.T) {
	h := r7Harness(t)
	profile := h.s.Profile()
	r7Fact(h, true, []int{0, 1}, profile, map[int]float64{0: 1.08, 1: 1.1}, []int{1}, nil, nil, nil)
	req, sources := h.s.r7Requests(machine.Workloads(machine.R7)[0].ID, []int{0}, []int{-20, -30, -30, -30})
	if diff := cmp.Diff(map[int]float64{0: 1.116}, req); diff != "" {
		t.Fatal(diff)
	}
	if len(sources) != 1 {
		t.Fatal(sources)
	}
	top := h.s.r7Top(machine.Workloads(machine.R7)[1].ID, []int{0, 1}, profile)
	if diff := cmp.Diff([]int{0, 1}, top); diff != "" {
		t.Fatal(diff)
	}
}

func TestR7LiveNamedAndMCEAttribution(t *testing.T) {
	for _, localMCE := range []bool{false, true} {
		t.Run(fmt.Sprint(localMCE), func(t *testing.T) {
			h := r7Harness(t)
			tr := Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, DurationS: 120, Condition: machine.Together}
			intent := h.start(Action{Kind: RunTrial, Trial: tr})
			causes := []int{intent.Seq}
			end := journal.TrialEnd{Trial: intent.Data.(*journal.TrialIntent).Trial, Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, TopRequesters: []int{0}}
			if localMCE {
				mce := h.add(&journal.MCE{Core: 1, BankType: machine.LoadStore})
				causes = append(causes, mce.Seq)
			} else {
				end.Core = new(1)
			}
			h.add(&end, causes...)
			h.decide(h.next())
			a := h.next()
			move, ok := a.Payload.(*journal.TunerDecision)
			if !ok || move.Decision != journal.Backoff || move.Core != 1 {
				t.Fatalf("named core did not move: %+v", a)
			}
		})
	}
}
func TestR7UnattributedVoltageTarget(t *testing.T) {
	h := r7Harness(t)
	for range h.s.n {
		r7Fact(h, true, []int{0, 1}, []int{-26, -30, -30, -30}, map[int]float64{0: 1.115, 1: 1.08}, []int{0}, nil, nil, nil)
	}
	r7Fact(h, false, []int{0, 1}, h.s.Profile(), map[int]float64{0: 1.1, 1: 1.08}, []int{0}, nil, nil, nil)
	a, ok := h.s.Drain()
	if !ok {
		t.Fatal("no voltage backoff")
	}
	move, ok := a.Payload.(*journal.TunerDecision)
	if !ok || move.ToOffset != -25 {
		t.Fatalf("rounded voltage target: %+v", a)
	}
}

func TestR7NamedTargetTransfersAcrossLoadsWithoutFailClock(t *testing.T) {
	h := r7Harness(t)
	for range h.s.n {
		r7Fact(h, true, []int{0, 1, 2, 3}, []int{-30, -20, -30, -30}, map[int]float64{0: 1.09, 1: 1.118, 2: 1.1, 3: 1.09}, []int{1, 2}, nil, nil, nil)
	}
	r7Fact(h, false, []int{0, 1}, h.s.Profile(), map[int]float64{0: 1.1, 1: 1.09}, []int{0}, new(0), nil, nil)
	a, ok := h.s.Drain()
	if !ok {
		t.Fatal("no named target")
	}
	move, ok := a.Payload.(*journal.TunerDecision)
	if !ok || move.ToOffset != -25 {
		t.Fatalf("named cross-load target without clock: %+v", a)
	}
}
func TestR7AllCoreTargetsUseEachCCD(t *testing.T) {
	h := r7Harness(t)
	for range h.s.n {
		r7Fact(h, true, h.s.ids(), []int{-25, -30, -20, -30}, map[int]float64{0: 1.118, 1: 1.09, 2: 1.236, 3: 1.19}, []int{0, 2}, nil, nil, nil)
	}
	r7Fact(h, false, h.s.ids(), h.s.Profile(), map[int]float64{0: 1.1, 1: 1.09, 2: 1.2, 3: 1.19}, []int{0, 2}, nil, nil, nil)
	var moves []int
	for range 2 {
		a, ok := h.s.Drain()
		if !ok {
			t.Fatal("missing CCD backoff")
		}
		move, ok := a.Payload.(*journal.TunerDecision)
		if !ok || move.Decision != journal.Backoff {
			t.Fatalf("%+v", a)
		}
		moves = append(moves, move.ToOffset)
		h.decide(a)
	}
	if diff := cmp.Diff([]int{-25, -20}, moves); diff != "" {
		t.Fatal(diff)
	}
}

func TestR7ZeroFailureDrainsWithoutReadingRanking(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseHasRoom}, coreStart{phase: journal.PhaseHasRoom}, coreStart{phase: journal.PhaseHasRoom}, coreStart{phase: journal.PhaseHasRoom})
	h.add(&journal.ProfileChange{To: []int{0, 0, 0, 0}})
	r7Fact(h, false, []int{0, 1}, []int{0, 0, 0, 0}, nil, []int{0, 1}, nil, nil, nil)
	a, ok := h.s.Drain()
	dead, deadOK := a.Payload.(*journal.DeadEnd)
	if !ok || !deadOK || dead.Condition != journal.DeadEndFailureAtZero {
		t.Fatalf("zero failure was not drained: %+v", a)
	}
}

func TestR7BackoffReason(t *testing.T) {
	for _, tc := range []struct {
		name     string
		requests map[int]float64
		target   float64
		want     []string
		count    int
		noVolts  bool
	}{
		{"offset fallback", nil, 0, []string{"offsets", "CO -30", "no request telemetry"}, 1, true},
		{"no pass", map[int]float64{0: 1.09447, 1: 1.08}, 0, []string{"request 1.094 V", "no qualifying pass"}, 1, false},
		{"equal target", map[int]float64{0: 1.1539, 1: 1.08}, 1.1539, []string{"request 1.154 V", "already met", "passed target 1.154 V"}, 1, false},
		{"higher than target", map[int]float64{0: 1.16, 1: 1.08}, 1.1539, []string{"request 1.160 V", "already met", "passed target 1.154 V"}, 1, false},
		{"one count", map[int]float64{0: 1.15, 1: 1.08}, 1.153, []string{"request 1.150 V", "1.153 V"}, 1, false},
		{"several counts", map[int]float64{0: 1.09447, 1: 1.08}, 1.1539, []string{"request 1.094 V", "1.154 V"}, 17, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := r7Harness(t)
			if tc.target > 0 {
				for range config.Default().Evidence.Trials() {
					r7Fact(h, true, []int{0, 1}, h.s.Profile(), map[int]float64{0: tc.target, 1: 1.08}, []int{0}, nil, nil, nil)
				}
			}
			failure := r7Fact(h, false, []int{0, 1}, h.s.Profile(), tc.requests, []int{0}, new(0), nil, nil)
			a, ok := h.s.Drain()
			if !ok {
				t.Fatal("failure did not move")
			}
			move := a.Payload.(*journal.TunerDecision)
			tokens := append([]string{"voltage-targeted R7 backoff", fmt.Sprintf("failure #%d", failure.Seq), "core 00", "ruleset8"}, tc.want...)
			if missing := missingTokens(move.Reason, tokens...); len(missing) > 0 {
				t.Fatalf("reason %q lacks %q", move.Reason, missing)
			}
			unit := "counts"
			if tc.count == 1 {
				unit = "count"
			}
			if !regexp.MustCompile(fmt.Sprintf(`(?:^|\D)%d %s(?:[^A-Za-z]|$)`, tc.count, unit)).MatchString(move.Reason) {
				t.Fatalf("reason %q does not report exactly %d %s", move.Reason, tc.count, unit)
			}
			if tc.noVolts && regexp.MustCompile(`[0-9] V(?:[^A-Za-z]|$)`).MatchString(move.Reason) {
				t.Fatalf("reason %q prints a voltage without request telemetry", move.Reason)
			}
			if !slices.Contains(a.Cause, failure.Seq) {
				t.Fatalf("backoff cause %v omits failure #%d", a.Cause, failure.Seq)
			}
		})
	}
}

func TestR7ZeroTopStepsDownWithinCCD(t *testing.T) {
	for _, tc := range []struct {
		name    string
		profile []int
		req     map[int]float64
		want    int
	}{
		{"measured order", []int{0, -30, -30, -30}, map[int]float64{0: 1.2, 1: 1.15}, -16},
		{"offset fallback", []int{0, -30, -30, -30}, nil, -29},
		{"partly zero tie", []int{0, -30, -30, -30}, map[int]float64{0: 1.2, 1: 1.1995}, -29},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := r7Harness(t)
			h.add(&journal.ProfileChange{To: tc.profile})
			top := []int{0}
			if tc.req != nil {
				top = h.s.r7TopRequests([]int{0, 1}, tc.req)
			}
			f := r7Fact(h, false, []int{0, 1}, tc.profile, tc.req, top, nil, nil, nil)
			a, ok := h.s.Drain()
			move, moved := a.Payload.(*journal.TunerDecision)
			if !ok || !moved || move.Decision != journal.Backoff || move.Core != 1 {
				t.Fatalf("%+v", a)
			}
			if diff := cmp.Diff(tc.want, move.ToOffset); diff != "" {
				t.Fatal(diff)
			}
			if tc.name != "partly zero tie" && !strings.Contains(move.Reason, "step down request order") {
				t.Fatal(move.Reason)
			}
			if tc.req != nil && !slices.Contains(a.Cause, f.Seq) {
				t.Fatal(a.Cause)
			}
			h.decide(a)
			if _, pending := h.s.Drain(); pending {
				t.Fatal("same CCD failure moved twice")
			}
			assertProjectionReplay(h)
		})
	}
}

func TestR7ZeroTopUsesShiftedFullMeasurement(t *testing.T) {
	h := r7Harness(t)
	source := r7Fact(h, true, []int{0, 1}, []int{-5, -30, -30, -30}, map[int]float64{0: 1.2, 1: 1.15}, []int{0}, nil, nil, nil)
	profile := []int{0, -30, -30, -30}
	h.add(&journal.ProfileChange{To: profile})
	r7Fact(h, false, []int{0, 1}, profile, nil, []int{0}, nil, nil, nil)
	a, ok := h.s.Drain()
	move, moved := a.Payload.(*journal.TunerDecision)
	if !ok || !moved || move.Core != 1 {
		t.Fatalf("%+v", a)
	}
	if diff := cmp.Diff(-11, move.ToOffset); diff != "" {
		t.Fatal(diff)
	}
	if !slices.Contains(a.Cause, source.Seq) {
		t.Fatal(a.Cause)
	}
}

// runZeroRerun checks that the next action reruns the failing load with every core at CO 0, and answers it.
func runZeroRerun(h *harness, cores []int, fail bool) journal.Event {
	h.t.Helper()
	a := h.next()
	if !a.Trial.Rerun || a.Trial.Condition != machine.Parked || !allZero(a.Trial.Profile) || len(a.Trial.Profile) != len(h.s.cores) || !slices.Equal(a.Trial.Cores, cores) {
		h.t.Fatalf("all-zero rerun %+v", a)
	}
	if !fail {
		_, end := h.trial(a, passed)
		return end
	}
	_, end := h.trial(a, failed)
	h.decide(h.next())
	return end
}

func TestR7ZeroAffectedCCDDeadEndsAfterItsAllZeroRerun(t *testing.T) {
	for _, rerunFails := range []bool{true, false} {
		t.Run(fmt.Sprint(rerunFails), func(t *testing.T) {
			h := r7Harness(t)
			profile := []int{0, 0, -30, -30}
			h.add(&journal.ProfileChange{To: profile})
			failure := r7Fact(h, false, h.s.ids(), profile, map[int]float64{0: 1.2, 1: 1.15, 2: 1.3, 3: 1.25}, []int{0, 2}, nil, new(1), nil)
			if a, ok := h.s.Drain(); ok {
				t.Fatalf("decided before the all-zero rerun: %+v", a)
			}
			rerun := runZeroRerun(h, h.s.ids(), rerunFails)
			a, ok := h.s.Drain()
			if !rerunFails {
				move, moved := a.Payload.(*journal.TunerDecision)
				if !ok || !moved || move.Decision != journal.Backoff || move.Core != 2 || move.ToOffset != -29 {
					t.Fatalf("a passed rerun did not send the failure to CCD 1: %+v", a)
				}
				if !slices.Contains(a.Cause, failure.Seq) || !slices.Contains(a.Cause, rerun.Seq) {
					t.Fatalf("backoff cause %v must cite failure #%d and its passed rerun #%d", a.Cause, failure.Seq, rerun.Seq)
				}
				if missing := missingTokens(move.Reason, "unattributed", "CO 0", "passed", fmt.Sprintf("#%d", failure.Seq), fmt.Sprintf("#%d", rerun.Seq)); len(missing) > 0 {
					t.Fatalf("reason %q lacks %q", move.Reason, missing)
				}
				h.decide(a)
				if a, pending := h.s.Drain(); pending {
					t.Fatalf("the failure moved twice: %+v", a)
				}
				assertProjectionReplay(h)
				return
			}
			dead, ended := a.Payload.(*journal.DeadEnd)
			if !ok || !ended || dead.Condition != journal.DeadEndFailureAtZero || !slices.Contains(a.Cause, failure.Seq) || !slices.Contains(a.Cause, rerun.Seq) {
				t.Fatalf("%+v", a)
			}
			failedRerun := fmt.Sprintf("the rerun with every core at CO 0 failed too (#%d)", rerun.Seq)
			if missing := missingTokens(dead.Detail, "CCD 0", "[0 1]"); len(missing) > 0 || !appearsInOrder(dead.Detail, failedRerun, "so the instability is not caused by Curve Optimizer") {
				t.Fatalf("dead end detail %q lacks %q or the rerun phrase %q before its conclusion", dead.Detail, missing, failedRerun)
			}
		})
	}
}

func TestR7NamedZeroAttributionUsesStartTop(t *testing.T) {
	for _, tc := range []struct {
		name     string
		top      []int
		requests map[int]float64
		previous map[int]float64
		dead     bool
	}{
		{"measured top", []int{0}, map[int]float64{0: 1.2, 1: 1.1}, nil, true},
		{"measured tied top", []int{0, 1}, map[int]float64{0: 1.2, 1: 1.2005}, nil, true},
		{"measured not top", []int{1}, map[int]float64{0: 1.1, 1: 1.2}, nil, false},
		{"derived top", nil, nil, map[int]float64{0: 1.2, 1: 1.1}, true},
		{"derived not top", nil, nil, map[int]float64{0: 1.1, 1: 1.2}, false},
		{"offset fallback top", nil, nil, nil, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := r7Harness(t)
			profile := []int{0, -30, -30, -30}
			h.add(&journal.ProfileChange{To: profile})
			var previous journal.Event
			if tc.previous != nil {
				previous = r7Fact(h, true, []int{0, 1}, profile, tc.previous, nil, nil, nil, nil)
			}
			r7Fact(h, false, []int{0, 1}, profile, tc.requests, tc.top, new(0), nil, nil)
			if tc.name == "derived top" {
				r7Fact(h, true, []int{0, 1}, profile, map[int]float64{0: 1.1, 1: 1.2}, []int{1}, nil, nil, nil)
			}
			a, ok := h.s.Drain()
			var rerun journal.Event
			if tc.dead {
				if ok {
					t.Fatalf("decided before the all-zero rerun: %+v", a)
				}
				rerun = runZeroRerun(h, []int{0, 1}, true)
				a, ok = h.s.Drain()
			}
			if !ok {
				t.Fatal("no zero decision")
			}
			if tc.dead {
				dead, ended := a.Payload.(*journal.DeadEnd)
				if !ended || dead.Condition != journal.DeadEndFailureAtZero {
					t.Fatalf("%+v", a)
				}
				basis := "top requesters"
				switch {
				case tc.previous != nil:
					basis = fmt.Sprintf("[%d]", previous.Seq)
					if !slices.Contains(a.Cause, previous.Seq) {
						t.Fatalf("dead end lost the measurement that made core 00 top: %v", a.Cause)
					}
				case tc.top == nil:
					basis = "no request telemetry"
				}
				failedRerun := fmt.Sprintf("the rerun with every core at CO 0 failed too (#%d)", rerun.Seq)
				if missing := missingTokens(dead.Detail, "core 00", "CO 0", "top requester of CCD 0", basis); len(missing) > 0 || !appearsInOrder(dead.Detail, failedRerun, "so the instability is not caused by Curve Optimizer") {
					t.Fatalf("dead end detail %q lacks %q or the rerun phrase %q before its conclusion", dead.Detail, missing, failedRerun)
				}
			} else {
				move, moved := a.Payload.(*journal.TunerDecision)
				if !moved || move.Decision != journal.Backoff || move.Core != 1 || move.ToOffset != -29 {
					t.Fatalf("%+v", a)
				}
				if !strings.Contains(move.Reason, "without being a top requester") {
					t.Fatal(move.Reason)
				}
				h.decide(a)
				if _, pending := h.s.Drain(); pending {
					t.Fatal("named zero failure moved twice")
				}
			}
			assertProjectionReplay(h)
		})
	}
}

func TestR7StepDownTargetExceedsOriginalTop(t *testing.T) {
	h := r7Harness(t)
	for range h.s.n {
		r7Fact(h, true, []int{0, 1}, []int{0, -20, -30, -30}, map[int]float64{0: 1.19, 1: 1.15}, []int{0}, nil, nil, nil)
		r7Fact(h, true, []int{0, 1}, []int{0, -10, -30, -30}, map[int]float64{0: 1.236, 1: 1.15}, []int{0}, nil, nil, nil)
	}
	profile := []int{0, -30, -30, -30}
	h.add(&journal.ProfileChange{To: profile})
	r7Fact(h, false, []int{0, 1}, profile, map[int]float64{0: 1.2, 1: 1.15}, []int{0}, nil, nil, nil)
	a, ok := h.s.Drain()
	move, moved := a.Payload.(*journal.TunerDecision)
	if !ok || !moved || move.Core != 1 {
		t.Fatalf("%+v", a)
	}
	if diff := cmp.Diff(-6, move.ToOffset); diff != "" {
		t.Fatal(diff)
	}
}

func TestR7FailureUsesRequestsAsOfItsStart(t *testing.T) {
	h := r7Harness(t)
	cores := []int{0, 1}
	before := r7Fact(h, true, cores, h.s.Profile(), map[int]float64{0: 1.2, 1: 1.1}, []int{0}, nil, nil, nil)
	r7Fact(h, false, cores, h.s.Profile(), nil, nil, nil, nil, nil)
	after := r7Fact(h, true, cores, h.s.Profile(), map[int]float64{0: 1.1, 1: 1.2}, []int{1}, nil, nil, nil)
	a, ok := h.s.Drain()
	move, moved := a.Payload.(*journal.TunerDecision)
	if !ok || !moved || move.Decision != journal.Backoff || move.Core != 0 {
		t.Fatalf("the failure did not back off its trial's top requester: %+v", a)
	}
	if !slices.Contains(a.Cause, before.Seq) || slices.Contains(a.Cause, after.Seq) {
		t.Fatalf("cause %v must cite #%d and not the later #%d", a.Cause, before.Seq, after.Seq)
	}
	h.decide(a)
	if a, pending := h.s.Drain(); pending {
		t.Fatalf("one failure moved twice: %+v", a)
	}
}

func TestR7VoltageTargetCitesItsLowestPass(t *testing.T) {
	h := r7Harness(t)
	cores := []int{0, 1}
	passed := []int{-20, -30, -30, -30}
	for range h.s.n {
		r7Fact(h, true, cores, passed, map[int]float64{0: 1.136, 1: 1.08}, []int{0}, nil, nil, nil)
	}
	lowest := r7Fact(h, true, cores, passed, map[int]float64{0: 1.118, 1: 1.08}, []int{0}, nil, nil, nil)
	r7Fact(h, false, cores, h.s.Profile(), map[int]float64{0: 1.1, 1: 1.08}, []int{0}, nil, nil, nil)
	a, ok := h.s.Drain()
	move, moved := a.Payload.(*journal.TunerDecision)
	if !ok || !moved || move.Core != 0 {
		t.Fatalf("%+v", a)
	}
	if diff := cmp.Diff(-25, move.ToOffset); diff != "" {
		t.Fatal(diff)
	}
	if !slices.Contains(a.Cause, lowest.Seq) {
		t.Fatalf("cause %v omits the pass #%d that set the 1.118 V target", a.Cause, lowest.Seq)
	}
}

func TestR7BackoffCountsEachCarriedFactOnce(t *testing.T) {
	h := r7Harness(t)
	failure := r7Fact(h, false, []int{0, 1}, h.s.Profile(), map[int]float64{0: 1.1, 1: 1.08}, []int{0}, nil, nil, nil)
	a, ok := h.s.Drain()
	move, moved := a.Payload.(*journal.TunerDecision)
	if !ok || !moved {
		t.Fatalf("%+v", a)
	}
	if n := strings.Count(move.Reason, "ruleset8"); n != 1 || len(missingTokens(move.Reason, "1 carried")) > 0 {
		t.Fatalf("reason %q must count its one carried fact once and name its source session once", move.Reason)
	}
	if n := strings.Count(fmt.Sprint(a.Cause), fmt.Sprint(failure.Seq)); n != 1 {
		t.Fatalf("backoff cause %v cites failure #%d %d times", a.Cause, failure.Seq, n)
	}
}

func TestR7AllCoreFailureSkipsCCDAtZero(t *testing.T) {
	for _, tc := range []struct {
		name    string
		profile []int
		dead    bool
	}{
		{"other CCD movable", []int{0, 0, -30, -30}, false},
		{"every CCD at zero", []int{0, 0, 0, 0}, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := r7Harness(t)
			r7Fact(h, false, h.s.ids(), tc.profile, nil, []int{1, 2}, nil, nil, nil)
			a, ok := h.s.Drain()
			if !ok {
				t.Fatal("no decision")
			}
			if tc.dead {
				if dead, ended := a.Payload.(*journal.DeadEnd); !ended || dead.Condition != journal.DeadEndFailureAtZero {
					t.Fatalf("%+v", a)
				}
				return
			}
			move, moved := a.Payload.(*journal.TunerDecision)
			if !moved || move.Decision != journal.Backoff {
				t.Fatalf("%+v", a)
			}
			if diff := cmp.Diff([2]int{2, -29}, [2]int{move.Core, move.ToOffset}); diff != "" {
				t.Fatal(diff)
			}
			h.decide(a)
			if a, pending := h.s.Drain(); pending {
				t.Fatalf("CCD 0 at CO 0 still counted: %+v", a)
			}
			assertProjectionReplay(h)
		})
	}
}

func TestR7OffsetStandInMovesOneCount(t *testing.T) {
	for _, tc := range []struct {
		name        string
		earlier     map[int]float64
		cores       []int
		top         []int
		stalled     *int
		passProfile []int
		passes      map[int]float64
		core        int
	}{
		{"no measurement as of the failure", nil, []int{0, 1}, []int{0}, nil, []int{-26, -30, -30, -30}, map[int]float64{0: 1.115, 1: 1.08}, 0},
		{"measured CCD beside an unmeasured one", map[int]float64{0: 1.2, 1: 1.15}, []int{0, 1, 2, 3}, nil, new(3), []int{-30, -30, -26, -26}, map[int]float64{0: 1.1, 1: 1.08, 2: 1.2, 3: 1.19}, 3},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := r7Harness(t)
			if tc.earlier != nil {
				r7Fact(h, true, []int{0, 1}, h.s.Profile(), tc.earlier, []int{0}, nil, nil, nil)
			}
			r7Fact(h, false, tc.cores, h.s.Profile(), nil, tc.top, nil, tc.stalled, nil)
			var passes []int
			for range h.s.n {
				passes = append(passes, r7Fact(h, true, tc.cores, tc.passProfile, tc.passes, nil, nil, nil, nil).Seq)
			}
			a, ok := h.s.Drain()
			move, moved := a.Payload.(*journal.TunerDecision)
			if !ok || !moved || move.Decision != journal.Backoff {
				t.Fatalf("%+v", a)
			}
			if diff := cmp.Diff([2]int{tc.core, -29}, [2]int{move.Core, move.ToOffset}); diff != "" {
				t.Fatal(diff)
			}
			if slices.ContainsFunc(a.Cause, func(seq int) bool { return slices.Contains(passes, seq) }) {
				t.Fatalf("cause %v cites passes %v as a voltage target for an offset stand-in", a.Cause, passes)
			}
		})
	}
}

func TestR7FailureNeedsNoMoveWhileMovedCoreIsShallower(t *testing.T) {
	for _, tc := range []struct {
		name            string
		current, failed []int
		named           *int
		round           bool
	}{
		{"named core shallower", []int{-30, -30, -30, -30}, []int{-35, -30, -30, -30}, new(0), false},
		{"top requester shallower", []int{-30, -30, -30, -30}, []int{-35, -35, -30, -30}, nil, false},
		{"top requester now at CO 0", []int{0, -30, -30, -30}, []int{-30, -30, -30, -30}, nil, false},
		{"during a deepening round", []int{-30, -30, -30, -30}, []int{-35, -30, -30, -30}, new(0), true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := r7Harness(t)
			setOffset := func(id, offset int) {
				h.add(&journal.CorePhase{Core: id, To: journal.PhaseHasRoom, Offset: offset, Reason: "test"})
			}
			for id, offset := range tc.current {
				if offset != h.s.core(id).offset {
					setOffset(id, offset)
				}
			}
			r7Fact(h, false, []int{0, 1}, tc.failed, nil, []int{0}, tc.named, nil, nil)
			if tc.round {
				h.add(&journal.DeepeningRound{Round: 1, Event: journal.CycleStart, Profile: h.s.offsets(), Target: h.s.offsets(), Cores: []int{1}, Trials: 2, TrialS: 120})
			}
			if a, ok := h.s.Drain(); ok {
				t.Fatalf("core 00 already shallower than in the failed trial, yet %+v", a)
			}
			setOffset(0, tc.failed[0])
			a, ok := h.s.Drain()
			if !ok {
				t.Fatal("failure took no effect once core 00 returned to its failing offset")
			}
			if tc.round {
				if end, ended := a.Payload.(*journal.DeepeningRound); !ended || end.Event != journal.CycleEnd {
					t.Fatalf("%+v", a)
				}
				return
			}
			move, moved := a.Payload.(*journal.TunerDecision)
			if !moved || move.Decision != journal.Backoff {
				t.Fatalf("%+v", a)
			}
			if diff := cmp.Diff([2]int{0, tc.failed[0] + 1}, [2]int{move.Core, move.ToOffset}); diff != "" {
				t.Fatal(diff)
			}
			assertProjectionReplay(h)
		})
	}
}

func TestR7NamedIdleZeroCoreCountsAgainstLoadedCCD(t *testing.T) {
	for _, tc := range []struct {
		name    string
		profile []int
		locates bool
	}{
		{"loaded CCD movable", []int{-30, -30, 0, -30}, false},
		{"loaded CCD at zero", []int{0, 0, 0, -30}, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := r7Harness(t)
			r7Fact(h, false, []int{0, 1}, tc.profile, nil, []int{0}, new(2), nil, nil)
			a, ok := h.s.Drain()
			if tc.locates {
				// With every loaded core at 0, the locate with every core at 0 is the failure's all-zero rerun.
				if ok {
					t.Fatalf("decided before locating: %+v", a)
				}
				a = h.next()
				start, started := a.Payload.(*journal.HuntStart)
				if !started || !slices.Equal(start.Parked, []int{0, 0, 0, 0}) || !slices.Equal(start.Candidates, []int{3}) {
					t.Fatalf("located hunt %+v", a)
				}
				if missing := missingTokens(start.Reason, "every loaded core", "CO 0", "cannot help"); len(missing) > 0 {
					t.Fatalf("carried hunt reason %q lacks %q", start.Reason, missing)
				}
				return
			}
			if !ok {
				t.Fatal("no decision")
			}
			move, moved := a.Payload.(*journal.TunerDecision)
			if !moved || move.Decision != journal.Backoff {
				t.Fatalf("%+v", a)
			}
			if diff := cmp.Diff([2]int{0, -29}, [2]int{move.Core, move.ToOffset}); diff != "" {
				t.Fatal(diff)
			}
			h.decide(a)
			if a, pending := h.s.Drain(); pending {
				t.Fatalf("one failure moved twice: %+v", a)
			}
			assertProjectionReplay(h)
		})
	}
}

// failLiveR7 runs CCD 0's load at the current profile until it fails with end, then records the failure.
func failLiveR7(h *harness, end journal.TrialEnd) (journal.Event, journal.Event) {
	h.t.Helper()
	tr := Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, DurationS: 120, Phase: journal.PhaseChecking, Condition: machine.Together}
	end.Outcome, end.Signal = journal.OutcomeFailure, machine.ComputationError
	_, ended := h.trial(Action{Kind: RunTrial, Trial: tr}, end)
	a := h.next()
	if f, ok := a.Payload.(*journal.Failure); !ok || (end.Core == nil) != (f.Attribution == journal.Unattributed) {
		h.t.Fatalf("attribution %+v", a)
	}
	return ended, h.decide(a)
}

// runLocated answers the located hunt's parked trials, failing those whose profile fails names, until the next
// action is neither a hunt group nor one of its trials.
func runLocated(h *harness, fails func([]int) *int) Action {
	h.t.Helper()
	return runLocatedUntil(h, func(t Trial) *int { return fails(t.Profile) }, func(Action) bool { return false })
}

// runLocatedUntil is runLocated over the whole parked trial, stopping before deciding a hunt event that stop accepts.
func runLocatedUntil(h *harness, fails func(Trial) *int, stop func(Action) bool) Action {
	h.t.Helper()
	for range 200 {
		a := h.next()
		switch a.Payload.(type) {
		case *journal.HuntStart, *journal.HuntGroup:
			if stop(a) {
				return a
			}
			h.decide(a)
			continue
		}
		if a.Kind != RunTrial || a.Trial.Condition != machine.Parked {
			return a
		}
		named := fails(a.Trial)
		if named == nil {
			end := passed
			end.DurationS = a.Trial.DurationS
			h.trial(a, end)
			continue
		}
		end := failed
		if *named >= 0 {
			end.Core = named
		}
		h.trial(a, end)
		h.decide(h.next())
	}
	h.t.Fatal("located hunt did not end")
	return Action{}
}

func unnamed(fail bool) *int {
	if fail {
		return new(-1)
	}
	return nil
}

func TestR7EscalatedFailureLocatesWithIdleCoresAtZero(t *testing.T) {
	h := r7Harness(t)
	ended, failure := failEscalatedR7(h, journal.TrialEnd{DurationS: 41, TopRequesters: []int{0}})
	backoffs := h.s.pendingFailures[len(h.s.pendingFailures)-1].loadBackoffs
	if len(backoffs) != escalateAfter {
		t.Fatalf("backoffs before the escalated failure %+v", backoffs)
	}
	a := h.next()
	reason := fmt.Sprintf("no core is named, so the failure is located on the unloaded cores before it is charged to the loaded cores; this load has been backed off 2 times since its last passing trial ([#%d #%d]) and still fails, so stepping back its loaded cores is not helping", backoffs[0].decision, backoffs[1].decision)
	want := &journal.HuntStart{Hunt: 1, Failure: failure.Seq, Trial: h.s.intents[ended.Data.(*journal.TrialEnd).Trial].Trial, Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Cores: []int{0, 1}, DurationS: 120, Failing: []int{-30, -30, -30, -30}, Parked: []int{-30, -30, 0, 0}, Candidates: []int{2, 3}, Trials: h.s.n, TrialS: 120, Miss: h.s.evidence.Miss, Rate: h.s.evidence.Rate, Ranking: []int{0, 1, 2, 3}, Reason: reason}
	if diff := cmp.Diff(Action{Kind: Decide, Payload: want, Cause: []int{failure.Seq, backoffs[0].decision, backoffs[1].decision}}, a); diff != "" {
		t.Fatalf("hunt start (-want +got):\n%s", diff)
	}
	h.decide(a)
	a = h.next()
	group, ok := a.Payload.(*journal.HuntGroup)
	if !ok || group.Stage != "locate" || len(group.Cores) != 0 || group.DurationS != 120 || !slices.Equal(group.Profile, []int{-30, -30, 0, 0}) {
		t.Fatalf("locate group %+v", a)
	}
	h.decide(a)
	trial := Trial{Regime: machine.R7, Workload: machine.Workloads(machine.R7)[0].ID, Condition: machine.Parked, Phase: journal.PhaseHunt, DurationS: 120, Cores: []int{0, 1}, Profile: []int{-30, -30, 0, 0}, Hunt: 1, Group: 1}
	trial.Requirement = ScheduledRequirement{Kind: "hunt", Class: ScheduledClass{Regime: trial.Regime, Workload: trial.Workload, Cores: "[0 1]", DurationS: trial.DurationS}, Since: h.events[len(h.events)-1].Seq, Rule: huntEvidence, Needed: h.s.n}
	if diff := cmp.Diff(trial, h.next().Trial); diff != "" {
		t.Fatalf("locate trial (-want +got):\n%s", diff)
	}
	if _, pending := h.s.Drain(); pending {
		t.Fatal("the failure moved a core while its located hunt was open")
	}
	assertProjectionReplay(h)
}

func TestR7PassedLocateHuntsTheUnloadedCulprit(t *testing.T) {
	h := r7Harness(t)
	failEscalatedR7(h, journal.TrialEnd{DurationS: 41, TopRequesters: []int{0}})
	a := runLocated(h, func(p []int) *int { return unnamed(p[2] != 0) })
	end, ok := a.Payload.(*journal.HuntEnd)
	if !ok || end.Result != "culprit" || !slices.Equal(end.Cores, []int{2}) {
		t.Fatalf("hunt end %+v", a)
	}
	h.decide(a)
	a = h.next()
	move, ok := a.Payload.(*journal.TunerDecision)
	if !ok || move.Decision != journal.Backoff || move.Phase != journal.PhaseHunt || move.Core != 2 || move.ToOffset != -29 {
		t.Fatalf("culprit backoff %+v", a)
	}
	h.decide(a)
	for a = h.next(); a.Kind == Decide; a = h.next() {
		if move, ok := a.Payload.(*journal.TunerDecision); ok {
			t.Fatalf("the located failure moved another core: %+v", move)
		}
		h.decide(a)
	}
	if !a.Trial.Rerun || a.Trial.Condition != machine.Together || !slices.Equal(a.Trial.Cores, []int{0, 1}) || a.Trial.DurationS != 120 {
		t.Fatalf("the failed CCD 0 load was not rerun: %+v", a)
	}
	if diff := cmp.Diff([]int{-30, -30, -29, -30}, h.s.offsets()); diff != "" {
		t.Fatalf("offsets (-want +got):\n%s", diff)
	}
	assertProjectionReplay(h)
}

func TestR7FailedLocateKeepsTheVoltageTargetedBackoff(t *testing.T) {
	h := r7Harness(t)
	for range h.s.n {
		r7Fact(h, true, []int{0, 1}, []int{-26, -30, -30, -30}, map[int]float64{0: 1.115, 1: 1.08}, []int{0}, nil, nil, nil)
	}
	_, failure := failEscalatedR7(h, journal.TrialEnd{DurationS: 41, TopRequesters: []int{0}, VoltageRequestsV: map[int]float64{0: 1.1, 1: 1.08}})
	var locate int
	a := runLocated(h, func(p []int) *int {
		locate = len(h.events) + 2
		return unnamed(true)
	})
	end, ok := a.Payload.(*journal.HuntEnd)
	if !ok || end.Result != "loaded" || !slices.Equal(end.Cores, []int{0, 1}) {
		t.Fatalf("hunt end %+v", a)
	}
	ended := h.decide(a)
	a = h.next()
	move, ok := a.Payload.(*journal.TunerDecision)
	if !ok || move.Decision != journal.Backoff || move.Core != 0 || move.ToOffset != -25 {
		t.Fatalf("voltage-targeted backoff %+v", a)
	}
	for _, seq := range []int{failure.Seq, ended.Seq, locate} {
		if !slices.Contains(a.Cause, seq) {
			t.Fatalf("backoff cause %v omits #%d", a.Cause, seq)
		}
	}
	if missing := missingTokens(move.Reason, "hunt 1", fmt.Sprintf("#%d", locate), "loaded cores", "unloaded core", "CO 0"); len(missing) > 0 {
		t.Fatalf("reason %q lacks %q", move.Reason, missing)
	}
	h.decide(a)
	if _, pending := h.s.Drain(); pending {
		t.Fatal("the failure moved twice")
	}
	assertProjectionReplay(h)
}

func TestR7LocatedGroupFailureNamingACore(t *testing.T) {
	for _, tc := range []struct {
		name   string
		named  int
		result string
		cores  []int
	}{
		{"loaded core", 0, "loaded", []int{0, 1}},
		{"unloaded core at its failing offset", 2, "direct", []int{2}},
		{"unloaded core parked at 0", 3, "culprit", []int{2}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := r7Harness(t)
			failEscalatedR7(h, journal.TrialEnd{DurationS: 41, TopRequesters: []int{0}})
			a := runLocated(h, func(p []int) *int {
				if p[2] == 0 {
					return nil
				}
				return new(tc.named)
			})
			end, ok := a.Payload.(*journal.HuntEnd)
			if !ok || end.Result != tc.result || !slices.Equal(end.Cores, tc.cores) {
				t.Fatalf("hunt end %+v", a)
			}
			h.decide(a)
			a = h.next()
			move, ok := a.Payload.(*journal.TunerDecision)
			if want := tc.cores[0]; !ok || move.Decision != journal.Backoff || move.Core != want || move.ToOffset != -29 {
				t.Fatalf("backoff %+v, want core %02d to -29", a, want)
			}
			h.decide(a)
			if _, pending := h.s.Drain(); pending {
				t.Fatal("the failure moved twice")
			}
			assertProjectionReplay(h)
		})
	}
}

func TestR7ZeroLoadedCCDDeadEndsOnlyAfterFailedLocate(t *testing.T) {
	for _, locateFails := range []bool{true, false} {
		t.Run(fmt.Sprint(locateFails), func(t *testing.T) {
			h := r7Harness(t)
			for id := range 2 {
				h.add(&journal.CorePhase{Core: id, To: journal.PhaseHasRoom, Offset: 0, Reason: "test"})
			}
			h.add(&journal.ProfileChange{To: []int{0, 0, -30, -30}})
			_, failure := failLiveR7(h, journal.TrialEnd{DurationS: 41, TopRequesters: []int{0}})
			if _, pending := h.s.Drain(); pending {
				t.Fatal("dead-ended before locating")
			}
			var locate int
			a := runLocated(h, func(p []int) *int {
				locate = len(h.events) + 2
				return unnamed(locateFails || p[3] != 0)
			})
			end, ok := a.Payload.(*journal.HuntEnd)
			if !locateFails {
				if !ok || end.Result != "culprit" || !slices.Equal(end.Cores, []int{3}) {
					t.Fatalf("hunt end %+v", a)
				}
				return
			}
			if !ok || end.Result != "loaded" {
				t.Fatalf("hunt end %+v", a)
			}
			h.decide(a)
			a = h.next()
			dead, ok := a.Payload.(*journal.DeadEnd)
			if !ok || dead.Condition != journal.DeadEndFailureAtZero {
				t.Fatalf("%+v", a)
			}
			if !slices.Contains(a.Cause, failure.Seq) || !slices.Contains(a.Cause, locate) {
				t.Fatalf("dead end cause %v must cite failures #%d and #%d", a.Cause, failure.Seq, locate)
			}
			if missing := missingTokens(dead.Detail, "CCD 0", "[0 1]", "CO 0", "hunt 1", fmt.Sprintf("#%d", locate), "the instability is not caused by Curve Optimizer"); len(missing) > 0 {
				t.Fatalf("dead end detail %q lacks %q", dead.Detail, missing)
			}
		})
	}
}

func TestR7LocatedFailureNamingALoadedCoreChargesThatCore(t *testing.T) {
	for _, tc := range []struct {
		name   string
		fails  func([]int) bool
		groups int
	}{
		{"locate", func([]int) bool { return true }, 1},
		{"narrowing group", func(p []int) bool { return p[2] != 0 || p[3] != 0 }, 2},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := r7Harness(t)
			_, failure := failEscalatedR7(h, journal.TrialEnd{DurationS: 41, TopRequesters: []int{0}})
			var named int
			a := runLocated(h, func(p []int) *int {
				if !tc.fails(p) {
					return nil
				}
				named = len(h.events) + 2
				return new(1)
			})
			end, ok := a.Payload.(*journal.HuntEnd)
			if !ok || end.Result != "loaded" || !slices.Equal(end.Cores, []int{0, 1}) || end.Groups != tc.groups {
				t.Fatalf("hunt end %+v", a)
			}
			ended := h.decide(a)
			a = h.next()
			move, ok := a.Payload.(*journal.TunerDecision)
			if !ok || move.Decision != journal.Backoff || move.Core != 1 || move.FromOffset != -30 || move.ToOffset != -29 || move.FailurePoint == nil || *move.FailurePoint != -30 {
				t.Fatalf("named loaded core backoff %+v, want core 01 from -30 to -29 with failure point -30", a)
			}
			if a.Cause[0] != failure.Seq {
				t.Fatalf("backoff cause %v must cite the hunted failure #%d first", a.Cause, failure.Seq)
			}
			for _, seq := range []int{ended.Seq, named} {
				if !slices.Contains(a.Cause, seq) {
					t.Fatalf("backoff cause %v omits #%d", a.Cause, seq)
				}
			}
			if missing := missingTokens(move.Reason, "hunt 1", "loaded core 01", fmt.Sprintf("#%d", named), fmt.Sprintf("#%d", failure.Seq)); len(missing) > 0 {
				t.Fatalf("reason %q lacks %q", move.Reason, missing)
			}
			h.decide(a)
			if _, pending := h.s.Drain(); pending {
				t.Fatal("the hunted failure moved a second core")
			}
			for a = h.next(); a.Kind == Decide; a = h.next() {
				if move, ok := a.Payload.(*journal.TunerDecision); ok {
					t.Fatalf("the located failure moved another core: %+v", move)
				}
				h.decide(a)
			}
			if a.Kind != RunTrial || a.Trial.Profile != nil && a.Trial.Profile[1] <= -30 {
				t.Fatalf("next trial %+v reapplies core 01 at its failure point", a)
			}
			if diff := cmp.Diff([]int{-30, -29, -30, -30}, h.s.offsets()); diff != "" {
				t.Fatalf("offsets (-want +got):\n%s", diff)
			}
			if _, reaches := h.s.reaches(h.s.offsets()); reaches {
				t.Fatal("the offsets reach a recorded failure point")
			}
			if _, reaches := h.s.reaches([]int{-30, -30, -30, -30}); !reaches {
				t.Fatal("core 01's failure point at -30 was not recorded")
			}
			assertProjectionReplay(h)
		})
	}
}

func TestR7PassedAllZeroLocateNeverChargesLoadedCoresAtZero(t *testing.T) {
	for _, tc := range []struct {
		name   string
		fails  func([]int) *int
		result string
		cores  []int
	}{
		{"no group fails", func([]int) *int { return nil }, "fallback", []int{2, 3}},
		{"a group names a loaded core at 0", func(p []int) *int {
			if p[3] == 0 {
				return nil
			}
			return new(0)
		}, "culprit", []int{3}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := r7Harness(t)
			for id := range 2 {
				h.add(&journal.CorePhase{Core: id, To: journal.PhaseHasRoom, Offset: 0, Reason: "test"})
			}
			h.add(&journal.ProfileChange{To: []int{0, 0, -30, -30}})
			failLiveR7(h, journal.TrialEnd{DurationS: 41, TopRequesters: []int{0}})
			a := runLocated(h, tc.fails)
			end, ok := a.Payload.(*journal.HuntEnd)
			if !ok || end.Result != tc.result || !slices.Equal(end.Cores, tc.cores) {
				t.Fatalf("hunt end %+v", a)
			}
			h.decide(a)
			for a = h.next(); a.Kind == Decide; a = h.next() {
				if dead, ok := a.Payload.(*journal.DeadEnd); ok {
					t.Fatalf("dead-ended after the all-zero locate passed: %+v", dead)
				}
				h.decide(a)
			}
			if !slices.Equal(h.s.offsets()[:2], []int{0, 0}) {
				t.Fatalf("offsets %v", h.s.offsets())
			}
			assertProjectionReplay(h)
		})
	}
}

// TestR7LocateFailureNamingALoadedCoreAtZero fails a locate whose loaded cores were not all at CO 0, naming the
// loaded core at CO 0: that group failure follows the named-core rules, so a top requester first needs its own
// all-zero rerun and any other core routes the failure to its CCD's top group.
func TestR7LocateFailureNamingALoadedCoreAtZero(t *testing.T) {
	for _, tc := range []struct {
		name       string
		top        []int
		rerunFails bool
	}{
		{"top requester, rerun passes", []int{0}, false},
		{"top requester, rerun fails", []int{0}, true},
		{"not a top requester", []int{1}, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := r7Harness(t)
			h.add(&journal.CorePhase{Core: 0, To: journal.PhaseHasRoom, Offset: 0, Reason: "test"})
			h.add(&journal.ProfileChange{To: []int{0, -30, -30, -30}})
			_, failure := failEscalatedR7(h, journal.TrialEnd{DurationS: 41, TopRequesters: tc.top})
			a := h.next()
			for ; ; a = h.next() {
				switch a.Payload.(type) {
				case *journal.HuntStart, *journal.HuntGroup:
					h.decide(a)
					continue
				}
				break
			}
			if a.Kind != RunTrial || a.Trial.Hunt != 1 || !slices.Equal(a.Trial.Profile, []int{0, -30, 0, 0}) {
				t.Fatalf("locate trial %+v", a)
			}
			h.trial(a, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, DurationS: 41, Core: new(0), TopRequesters: tc.top})
			locate := h.decide(h.next())
			a = h.next()
			if end, ok := a.Payload.(*journal.HuntEnd); !ok || end.Result != "loaded" || !strings.Contains(end.Reason, "named loaded core 00") {
				t.Fatalf("hunt end %+v", a)
			}
			h.decide(a)
			a = h.next()
			if tc.top[0] != 0 {
				move, ok := a.Payload.(*journal.TunerDecision)
				if !ok || move.Decision != journal.Backoff || move.Core != 1 || move.ToOffset != -29 {
					t.Fatalf("CCD 0 top group backoff %+v", a)
				}
				if a.Cause[0] != failure.Seq || !slices.Contains(a.Cause, locate.Seq) {
					t.Fatalf("backoff cause %v must cite the hunted failure #%d first and the locate failure #%d", a.Cause, failure.Seq, locate.Seq)
				}
				if missing := missingTokens(move.Reason, "core 00", "CO 0", "top requester", "CCD 0"); len(missing) > 0 {
					t.Fatalf("reason %q lacks %q", move.Reason, missing)
				}
				h.decide(a)
				if a, pending := h.s.Drain(); pending {
					t.Fatalf("the failure moved twice: %+v", a)
				}
				assertProjectionReplay(h)
				return
			}
			if !a.Trial.Rerun || a.Trial.Condition != machine.Parked || a.Trial.Hunt != 0 || !allZero(a.Trial.Profile) || !slices.Equal(a.Trial.Cores, []int{0, 1}) || !slices.Equal(a.Cause, []int{locate.Seq}) {
				t.Fatalf("all-zero rerun of the locate failure %+v", a)
			}
			if tc.rerunFails {
				_, rerun := h.trial(a, failed)
				h.decide(h.next())
				a = h.next()
				dead, ok := a.Payload.(*journal.DeadEnd)
				if !ok || dead.Condition != journal.DeadEndFailureAtZero || !slices.Contains(a.Cause, locate.Seq) || !slices.Contains(a.Cause, rerun.Seq) {
					t.Fatalf("dead end %+v", a)
				}
				if want := fmt.Sprintf("the rerun with every core at CO 0 failed too (#%d)", rerun.Seq); !strings.Contains(dead.Detail, want) {
					t.Fatalf("detail %q lacks %q", dead.Detail, want)
				}
				assertProjectionReplay(h)
				return
			}
			_, rerun := h.trial(a, passed)
			a = h.next()
			move, ok := a.Payload.(*journal.TunerDecision)
			if !ok || move.Decision != journal.Backoff || move.Core != 1 || move.ToOffset != -29 {
				t.Fatalf("backoff after the passed rerun %+v", a)
			}
			for _, seq := range []int{failure.Seq, locate.Seq, rerun.Seq} {
				if !slices.Contains(a.Cause, seq) {
					t.Fatalf("backoff cause %v omits #%d", a.Cause, seq)
				}
			}
			h.decide(a)
			if a, pending := h.s.Drain(); pending {
				t.Fatalf("the failure moved twice: %+v", a)
			}
			assertProjectionReplay(h)
		})
	}
}

// TestR7NamedUnloadedCoreOffZeroIsNotLocated names a nonzero unloaded core while every loaded core is at CO 0: the
// failure counts against the named core, so no located hunt starts.
func TestR7NamedUnloadedCoreOffZeroIsNotLocated(t *testing.T) {
	h := r7Harness(t)
	for id := range 2 {
		h.add(&journal.CorePhase{Core: id, To: journal.PhaseHasRoom, Offset: 0, Reason: "test"})
	}
	h.add(&journal.ProfileChange{To: []int{0, 0, -30, -30}})
	_, failure := failLiveR7(h, journal.TrialEnd{DurationS: 41, TopRequesters: []int{0}, Core: new(2)})
	a := h.next()
	move, ok := a.Payload.(*journal.TunerDecision)
	if !ok || move.Decision != journal.Backoff || move.Core != 2 || move.ToOffset != -29 || a.Cause[0] != failure.Seq {
		t.Fatalf("named core 02 backoff %+v", a)
	}
	h.decide(a)
	if _, pending := h.s.Drain(); pending {
		t.Fatal("the failure moved twice")
	}
	if len(h.s.located) != 0 {
		t.Fatalf("located hunts %+v", h.s.located)
	}
	assertProjectionReplay(h)
}

// TestR7PassedZeroRerunLocatesCitingTheRerun names top requester core 0 at CO 0 in a live CCD 0 load: its all-zero
// rerun passes, so the failure names no core and is located, and hunt.start cites that rerun.
func TestR7PassedZeroRerunLocatesCitingTheRerun(t *testing.T) {
	h := r7Harness(t)
	h.add(&journal.CorePhase{Core: 0, To: journal.PhaseHasRoom, Offset: 0, Reason: "test"})
	h.add(&journal.ProfileChange{To: []int{0, -30, -30, -30}})
	_, failure := failEscalatedR7(h, journal.TrialEnd{DurationS: 41, TopRequesters: []int{0}, Core: new(0)})
	rerun := runZeroRerun(h, []int{0, 1}, false)
	a := h.next()
	start, ok := a.Payload.(*journal.HuntStart)
	if !ok || start.Failure != failure.Seq || !slices.Equal(start.Candidates, []int{2, 3}) {
		t.Fatalf("located hunt %+v", a)
	}
	if diff := cmp.Diff([]int{failure.Seq, rerun.Seq}, a.Cause[:2]); diff != "" {
		t.Fatalf("hunt start cause (-want +got):\n%s", diff)
	}
	if missing := missingTokens(start.Reason, "CO 0", "passed", fmt.Sprintf("#%d", failure.Seq), fmt.Sprintf("#%d", rerun.Seq)); len(missing) > 0 {
		t.Fatalf("reason %q lacks %q", start.Reason, missing)
	}
	h.decide(a)
	assertProjectionReplay(h)
}

// failEscalatedR7 fails CCD 0's load once more after escalateAfter unattributed failures of it were each backed off
// without a passing trial between, then restores the offsets and profile those backoffs moved so that the escalated
// failure fails at the profile the harness had. The failure is located instead of backed off.
func failEscalatedR7(h *harness, end journal.TrialEnd) (journal.Event, journal.Event) {
	h.t.Helper()
	offsets, profile := h.s.offsets(), slices.Clone(h.s.checking.profile)
	prior := end
	prior.Core = nil
	for range escalateAfter {
		failLiveR7(h, prior)
		a := h.next()
		if move, ok := a.Payload.(*journal.TunerDecision); !ok || move.Decision != journal.Backoff {
			h.t.Fatalf("expected a voltage-targeted backoff before escalation, got %+v", a)
		}
		h.decide(a)
		for i, id := range h.s.ids() {
			if h.s.offsets()[i] != offsets[i] {
				h.add(&journal.CorePhase{Core: id, To: journal.PhaseHasRoom, Offset: offsets[i], Reason: "test"})
			}
		}
		h.add(&journal.ProfileChange{To: profile})
	}
	return failLiveR7(h, end)
}
