package tuner

import (
	"path/filepath"
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

// carriedJointHunt runs a session whose only checking regime is R6. A carried R6 failure at the starting profile skips
// the first trial and starts the hunt; live parked trials then fail whenever core 00 is at -30 or deeper and core 01 at
// -27 or deeper. It calls decided, if not nil, after each recorded decision, stops after the hunt's first backoff and
// returns the carried failure's sequence.
func carriedJointHunt(t *testing.T, decided func(*harness, journal.Event)) (*harness, int) {
	t.Helper()
	starts := make([]coreStart, 4)
	for i := range starts {
		starts[i] = coreStart{phase: journal.PhaseAtLimit, offset: -30, fail: new(-31)}
	}
	h := newHarness(t, starts...)
	cfg := config.Default()
	cfg.Checking.Cycle = []machine.Regime{machine.R6}
	h.add(&journal.ConfigLoaded{Path: config.DefaultPath, Config: snapshotConfig(cfg)})
	carried := 0
	for range 500 {
		a := h.next()
		if a.Kind == RunTrial {
			if carried == 0 {
				tr := a.Trial
				carried = h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "20261001T000000Z", Seq: 1, Trial: "0001", Evidence: EvidenceEpoch}, Class: journal.TrialClass{Regime: tr.Regime, Workload: tr.Workload, Cores: tr.Cores, DurationS: tr.DurationS}, Condition: tr.Condition, Phase: tr.Phase, Profile: h.s.Profile(), DurationS: 5, Outcome: journal.OutcomeFailure, Signal: machine.Crash}).Seq
				continue
			}
			profile := a.Trial.Profile
			if profile == nil {
				profile = h.s.Profile()
			}
			end := passed
			if profile[0] <= -30 && profile[1] <= -27 {
				end = journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 5}
			}
			h.trial(a, end)
			continue
		}
		e := h.decide(a)
		if decided != nil {
			decided(h, e)
		}
		if d, ok := a.Payload.(*journal.TunerDecision); ok && d.Phase == journal.PhaseHunt {
			return h, carried
		}
	}
	t.Fatal("the hunt never backed off")
	return nil, 0
}

// reaches walks cause links back from cause and returns every event it reaches.
func reaches(events []journal.Event, cause ...int) map[int]bool {
	seen := map[int]bool{}
	seqs := slices.Clone(cause)
	for len(seqs) > 0 {
		seq := seqs[len(seqs)-1]
		seqs = seqs[:len(seqs)-1]
		if seen[seq] || seq < 1 || seq > len(events) {
			continue
		}
		seen[seq] = true
		seqs = append(seqs, events[seq-1].Cause...)
	}
	return seen
}

// huntFailures returns the failing trial.end sequences of the hunt's parked trials.
func huntFailures(events []journal.Event, hunt int) []int {
	intents := map[string]*journal.TrialIntent{}
	var out []int
	for _, e := range events {
		switch p := e.Data.(type) {
		case *journal.TrialIntent:
			intents[p.Trial] = p
		case *journal.TrialEnd:
			if intent := intents[p.Trial]; intent != nil && intent.Hunt == hunt && p.Outcome == journal.OutcomeFailure {
				out = append(out, e.Seq)
			}
		}
	}
	return out
}

func TestHuntCommitmentsCiteDecisiveLiveFailures(t *testing.T) {
	h, carried := carriedJointHunt(t, nil)
	var start, end, combination, backoff journal.Event
	for _, e := range h.events {
		switch p := e.Data.(type) {
		case *journal.HuntStart:
			start = e
		case *journal.HuntEnd:
			end = e
		case *journal.Combination:
			combination = e
		case *journal.TunerDecision:
			if p.Phase == journal.PhaseHunt {
				backoff = e
			}
		}
	}
	if diff := cmp.Diff([]int{carried}, start.Cause); diff != "" {
		t.Fatalf("hunt should start from the carried failure (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff([]journal.CombinationMember{{Core: 0, Offset: -30}, {Core: 1, Offset: -27}}, end.Data.(*journal.HuntEnd).Members); diff != "" {
		t.Fatalf("member edges (-want +got):\n%s", diff)
	}
	// Live failures rejected the joint's part (#27) and core 01's probes at -29, -28 and -27. Each later failure is also
	// at a shallower profile than the part, so only the failure seen before the next group was planned identifies it.
	failures := huntFailures(h.events, start.Data.(*journal.HuntStart).Hunt)
	if diff := cmp.Diff(append([]int{start.Seq}, failures...), end.Cause); diff != "" {
		t.Fatalf("hunt.end cites the hunt and each rejecting live failure, no passes (-want +got):\n%s", diff)
	}
	for _, e := range []journal.Event{end, combination, backoff} {
		reached := reaches(h.events, e.Cause...)
		if !reached[carried] {
			t.Errorf("%s #%d cause %v does not reach the carried failure #%d", e.Kind, e.Seq, e.Cause, carried)
		}
		var missing []int
		for _, seq := range failures {
			if !reached[seq] {
				missing = append(missing, seq)
			}
		}
		if len(missing) > 0 {
			t.Errorf("%s #%d cause %v does not reach live failures %v", e.Kind, e.Seq, e.Cause, missing)
		}
	}
}

// A crash after group 2 is recorded, before its profile is applied, leaves an unattributed parked failure at group 1's
// profile. Group 1 had passed when the hunt moved on, and no later decision reads its outcome again, so no hunt decision
// may cite that failure as deciding it.
func TestHuntCausesOmitAFailureAfterTheGroupPassed(t *testing.T) {
	late := 0
	h, _ := carriedJointHunt(t, func(h *harness, e journal.Event) {
		if g, ok := e.Data.(*journal.HuntGroup); ok && g.Group == 2 && late == 0 {
			first := h.s.hunt.groups[0].payload
			if first.Probe != nil || h.s.groupOutcome(h.s.hunt, h.s.hunt.groups[0]) != "pass" {
				t.Fatalf("group 1 should be a passed part: %+v", first)
			}
			late = h.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Unattributed, Condition: machine.Parked, Regime: machine.R6, Profile: slices.Clone(first.Profile)}).Seq
		}
	})
	if late == 0 {
		t.Fatal("the hunt never recorded group 2")
	}
	for _, e := range h.events[late:] {
		switch p := e.Data.(type) {
		case *journal.HuntGroup, *journal.HuntEnd, *journal.Combination:
		case *journal.TunerDecision:
			if p.Phase != journal.PhaseHunt {
				continue
			}
		default:
			continue
		}
		if slices.Contains(e.Cause, late) {
			t.Errorf("%s #%d cause %v cites failure #%d, recorded after group 1 passed", e.Kind, e.Seq, e.Cause, late)
		}
	}
}

// A carried failure at group 1's profile arrives while it runs, followed by n carried passes that cover it, so the group
// passes on them. Every later hunt.group and hunt.end cites those passes for group 1, and no later hunt decision cites
// the covered failure.
func TestHuntCausesOmitAFailureCoveredBeforeTheGroupPassed(t *testing.T) {
	covered := 0
	var passes []int
	h, _ := carriedJointHunt(t, func(h *harness, e journal.Event) {
		g, ok := e.Data.(*journal.HuntGroup)
		if !ok || g.Group != 1 || covered != 0 {
			return
		}
		start := h.s.hunt.start
		carry := func(outcome journal.Outcome, signal machine.Signal) int {
			return h.add(&journal.TrialCarried{Source: journal.FactSource{Session: "20261001T000000Z", Seq: 2, Trial: "0002", Evidence: EvidenceEpoch}, Class: journal.TrialClass{Regime: start.Regime, Workload: start.Workload, Cores: start.Cores, DurationS: g.DurationS}, Condition: machine.Parked, Phase: journal.PhaseHunt, Profile: slices.Clone(g.Profile), DurationS: g.DurationS, Outcome: outcome, Signal: signal}).Seq
		}
		covered = carry(journal.OutcomeFailure, machine.Crash)
		for range h.s.n {
			passes = append(passes, carry(journal.OutcomePass, ""))
		}
		if h.s.groupOutcome(h.s.hunt, h.s.hunt.groups[0]) != "pass" {
			t.Fatal("the carried passes should pass group 1")
		}
	})
	if covered == 0 {
		t.Fatal("the hunt never recorded group 1")
	}
	var second int
	for _, e := range h.events {
		if g, ok := e.Data.(*journal.HuntGroup); ok && g.Group == 2 {
			second = e.Seq
			break
		}
	}
	for _, e := range h.events[second:] {
		switch p := e.Data.(type) {
		case *journal.HuntGroup, *journal.HuntEnd:
		case *journal.TunerDecision:
			if p.Phase != journal.PhaseHunt {
				continue
			}
		default:
			continue
		}
		if slices.Contains(e.Cause, covered) {
			t.Errorf("%s #%d cause %v cites failure #%d, covered before group 1 passed", e.Kind, e.Seq, e.Cause, covered)
		}
		if _, ok := e.Data.(*journal.TunerDecision); !ok && !slices.ContainsFunc(passes, func(seq int) bool { return slices.Contains(e.Cause, seq) }) {
			t.Errorf("%s #%d cause %v cites none of group 1's carried passes %v", e.Kind, e.Seq, e.Cause, passes)
		}
	}
}

func TestHuntEndCitesALiveFailureInferredGroup(t *testing.T) {
	h := newHarness(t, coreStart{phase: journal.PhaseAtLimit, offset: -10}, coreStart{phase: journal.PhaseAtLimit, offset: -10})
	d := h.s.durations.ShortTrialS
	workload := machine.Workloads(machine.R6)[0].ID
	_, failure := h.trial(Action{Kind: RunTrial, Trial: Trial{Regime: machine.R6, Workload: workload, Cores: []int{0, 1}, Condition: machine.Together, Phase: journal.PhaseChecking, DurationS: d, Profile: []int{-10, 0}}}, failed)
	start := h.add(&journal.HuntStart{Hunt: 1, Regime: machine.R6, Workload: workload, Cores: []int{0, 1}, DurationS: d, TrialS: d, Trials: h.s.n, Failing: []int{-10, -10}, Parked: []int{0, 0}, Candidates: []int{0, 1}})
	a, ok := h.s.huntNext()
	g, isGroup := a.Payload.(*journal.HuntGroup)
	if !ok || !isGroup || g.Inferred != "failure" || !slices.Equal(g.Profile, []int{-10, 0}) {
		t.Fatalf("the live failure should establish the part of core 00: %+v", a)
	}
	if diff := cmp.Diff([]int{start.Seq, failure.Seq}, a.Cause); diff != "" {
		t.Fatalf("inferred group cites its live failure (-want +got):\n%s", diff)
	}
	group := h.decide(a)
	a, ok = h.s.huntNext()
	if end, isEnd := a.Payload.(*journal.HuntEnd); !ok || !isEnd || end.Result != "culprit" {
		t.Fatalf("a failing singleton part ends the hunt: %+v", a)
	}
	if diff := cmp.Diff([]int{start.Seq, group.Seq}, a.Cause); diff != "" {
		t.Fatalf("hunt.end cites the group recording the inference, which cites the failure (-want +got):\n%s", diff)
	}
}

// The fixture is carriedJointHunt recorded by ruleset 9's tuner (revision 884be7d), before hunt commitments cited live
// failures: its hunt.end reaches the carried failure the hunt started from but none of the live failures that decided
// the joint and core 01's edge. Replaying it reaches the same hunt.end, now citing them.
func TestHuntLiveFailureCausesFixtureReplay(t *testing.T) {
	events, torn, err := journal.ReadFile(filepath.Join("testdata", "hunt-live-failure-causes.jsonl"))
	if err != nil {
		t.Fatal(err)
	}
	if len(torn) > 0 {
		t.Fatalf("fixture has a torn tail: %q", torn)
	}
	at := slices.IndexFunc(events, func(e journal.Event) bool { return e.Kind == journal.KindHuntEnd })
	if at < 0 {
		t.Fatal("fixture has no hunt.end")
	}
	end := events[at]
	failures := huntFailures(events, end.Data.(*journal.HuntEnd).Hunt)
	recorded := reaches(events, end.Cause...)
	for _, seq := range failures {
		if recorded[seq] {
			t.Fatalf("recorded hunt.end %v already reaches live failure #%d", end.Cause, seq)
		}
	}
	s := replayState(events[:at])
	a, ok := s.huntNext()
	if !ok {
		t.Fatal("replay did not end the hunt")
	}
	if diff := cmp.Diff(end.Data, a.Payload); diff != "" {
		t.Fatalf("replayed hunt.end decision (-recorded +replayed):\n%s", diff)
	}
	if diff := cmp.Diff(append([]int{s.hunt.seq}, failures...), a.Cause); diff != "" {
		t.Fatalf("replayed hunt.end cause (-want +got):\n%s", diff)
	}
}
