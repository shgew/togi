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
		r7Fact(h, false, cores, h.s.Profile(), map[int]float64{0: 1.1, 1: 1.08}, []int{0}, nil, nil, nil)
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
	h := r7Harness(t)
	h.s.rankingSeq = 0
	h.s.ranking = nil
	for _, c := range h.s.cores {
		c.offset = 0
	}
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
		want     string
	}{
		{"offset fallback", nil, 0, "order came from offsets at CO -30; no request telemetry, 1 count"},
		{"no pass", map[int]float64{0: 1.09447, 1: 1.08}, 0, "request 1.094 V; no qualifying pass, 1 count"},
		{"equal target", map[int]float64{0: 1.1539, 1: 1.08}, 1.1539, "request 1.154 V already met the passed target 1.154 V; 1 count"},
		{"higher than target", map[int]float64{0: 1.16, 1: 1.08}, 1.1539, "request 1.160 V already met the passed target 1.154 V; 1 count"},
		{"one count", map[int]float64{0: 1.15, 1: 1.08}, 1.153, "request 1.150 V to 1.153 V, 1 count"},
		{"several counts", map[int]float64{0: 1.09447, 1: 1.08}, 1.1539, "request 1.094 V to 1.154 V, 17 counts"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := r7Harness(t)
			h.s.n = 1
			if tc.target > 0 {
				r7Fact(h, true, []int{0, 1}, h.s.Profile(), map[int]float64{0: tc.target, 1: 1.08}, []int{0}, nil, nil, nil)
			}
			r7Fact(h, false, []int{0, 1}, h.s.Profile(), tc.requests, []int{0}, new(0), nil, nil)
			f := h.s.pendingFailures[len(h.s.pendingFailures)-1]
			a := h.s.r7Backoff(f, *h.s.r7FailureEntry(f), h.s.core(0), []int{f.seq}, r7Order{named: true})
			got := a.Payload.(*journal.TunerDecision).Reason
			want := fmt.Sprintf("voltage-targeted R7 backoff after failure #%d: core 00 %s", f.seq, tc.want) + h.s.carriedReason(a.Cause)
			if diff := cmp.Diff(want, got); diff != "" {
				t.Fatal(diff)
			}
		})
	}
}

func TestR7CountWording(t *testing.T) {
	for _, noun := range []string{"count", "failure", "start"} {
		if diff := cmp.Diff("1 "+noun, r7Count(1, noun)); diff != "" {
			t.Fatal(diff)
		}
		if diff := cmp.Diff("2 "+noun+"s", r7Count(2, noun)); diff != "" {
			t.Fatal(diff)
		}
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

func TestR7ZeroAffectedCCDDeadEndsRegardlessOfOtherCCD(t *testing.T) {
	h := r7Harness(t)
	profile := []int{0, 0, -30, -30}
	h.add(&journal.ProfileChange{To: profile})
	r7Fact(h, false, h.s.ids(), profile, map[int]float64{0: 1.2, 1: 1.15, 2: 1.3, 3: 1.25}, []int{0, 2}, nil, new(1), nil)
	a, ok := h.s.Drain()
	dead, ended := a.Payload.(*journal.DeadEnd)
	if !ok || !ended || dead.Condition != journal.DeadEndFailureAtZero {
		t.Fatalf("%+v", a)
	}
	want := "unattributed R7 failure counts against CCD 0's top group, and every loaded core of that CCD [0 1] is at CO 0; the instability is not caused by Curve Optimizer"
	if diff := cmp.Diff(want, dead.Detail); diff != "" {
		t.Fatal(diff)
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
			if !ok {
				t.Fatal("no zero decision")
			}
			if tc.dead {
				dead, ended := a.Payload.(*journal.DeadEnd)
				if !ended || dead.Condition != journal.DeadEndFailureAtZero {
					t.Fatalf("%+v", a)
				}
				basis := "its start's recorded top requesters"
				switch {
				case tc.previous != nil:
					basis = fmt.Sprintf("request measurements [%d]", previous.Seq)
					if !slices.Contains(a.Cause, previous.Seq) {
						t.Fatalf("dead end lost the measurement that made core 00 top: %v", a.Cause)
					}
				case tc.top == nil:
					basis = "offset order (no request telemetry)"
				}
				want := "core 00 failed at CO 0 as a top requester of CCD 0 by " + basis + "; the instability is not caused by Curve Optimizer"
				if diff := cmp.Diff(want, dead.Detail); diff != "" {
					t.Fatal(diff)
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
		t.Fatalf("the failure did not back off its start's top requester: %+v", a)
	}
	if !slices.Contains(a.Cause, before.Seq) || slices.Contains(a.Cause, after.Seq) {
		t.Fatalf("cause %v must cite #%d and not the later #%d", a.Cause, before.Seq, after.Seq)
	}
	h.decide(a)
	if a, pending := h.s.Drain(); pending {
		t.Fatalf("one failure moved twice: %+v", a)
	}
}

func TestR7BackoffCountsEachCarriedFactOnce(t *testing.T) {
	h := r7Harness(t)
	r7Fact(h, false, []int{0, 1}, h.s.Profile(), map[int]float64{0: 1.1, 1: 1.08}, []int{0}, nil, nil, nil)
	a, ok := h.s.Drain()
	move, moved := a.Payload.(*journal.TunerDecision)
	if !ok || !moved {
		t.Fatalf("%+v", a)
	}
	if want := h.s.carriedReason(a.Cause); !strings.HasSuffix(move.Reason, want) || !strings.Contains(want, "; 1 carried facts") {
		t.Fatalf("reason %q must end with %q for its one cited carried fact", move.Reason, want)
	}
}
