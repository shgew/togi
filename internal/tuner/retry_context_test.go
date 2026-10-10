package tuner

import (
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

// A saved retry belongs to the context it was scheduled for and runs only there; these tests drive each context through
// Fold, Next and Drain, then end it or change its requirement, and require the next trial to be the current context's
// own.

func requireTrial(t *testing.T, a Action) Trial {
	t.Helper()
	if a.Kind != RunTrial {
		t.Fatalf("action %+v, want a trial", a)
	}
	return a.Trial
}

// drainsNoTrial requires Drain to hold no trial, as a retry is one.
func drainsNoTrial(t *testing.T, s *State) {
	t.Helper()
	if a, ok := s.Drain(); ok && a.Kind == RunTrial {
		t.Fatalf("drain returned a trial: %+v", a)
	}
}

// nextRoundCheck decides what the round's moves require, as the live loop does, and returns its first alone check.
func nextRoundCheck(t *testing.T, h *harness, round int) Action {
	t.Helper()
	for range 30 {
		a := h.next()
		if a.Kind == RunTrial {
			if c := a.Trial; c.Round != round || c.Condition != machine.Alone || c.Core != 0 || c.Regime != machine.R1 {
				t.Fatalf("round %d check %+v", round, c)
			}
			return a
		}
		h.decide(a)
	}
	t.Fatalf("round %d never reached its check", round)
	return Action{}
}

func roundStartSeq(h *harness, round int) int {
	for _, e := range slices.Backward(h.events) {
		if p, ok := e.Data.(*journal.DeepeningRound); ok && p.Round == round && p.Event == journal.CycleStart {
			return e.Seq
		}
	}
	h.t.Fatalf("round %d never started", round)
	return 0
}

// roundCheckRetry opens round 1 of two cores through the live decisions and ends its first alone check inconclusively.
func roundCheckRetry(t *testing.T) (*harness, journal.Event) {
	t.Helper()
	h := cleanCycleHarness(t, []int{-10, -10}, nil)
	nextRound(h)
	begin := h.events[roundStartSeq(h, 1)-1]
	a := nextRoundCheck(t, h, 1)
	h.trial(a, unsure)
	if again := requireTrial(t, h.next()); !again.Retry || again.Round != 1 || again.Core != 0 {
		t.Fatalf("the round's own check did not retry: %+v", again)
	}
	return h, begin
}

func TestCancelledRoundCheckRetryDoesNotRun(t *testing.T) {
	t.Run("reset of another core ends the round", func(t *testing.T) {
		h, _ := roundCheckRetry(t)
		h.add(&journal.CommandReset{Core: new(1)})
		drainsNoTrial(t, h.s)
		a := h.next()
		if p, ok := a.Payload.(*journal.DeepeningRound); !ok || p.Event != journal.CycleEnd {
			t.Fatalf("reset must cancel the round first: %+v", a)
		}
		h.decide(a)
		a = h.next()
		if p, ok := a.Payload.(*journal.CorePhase); !ok || p.Core != 1 || p.To != journal.PhaseSearch {
			t.Fatalf("reset core did not restart its search: %+v", a)
		}
		h.decide(a)
		var got Action
		for range 10 {
			got = h.next()
			if got.Kind == RunTrial {
				break
			}
			h.decide(got)
		}
		next := requireTrial(t, got)
		if next.Retry || next.Round != 0 || next.Core != 1 || next.Phase != journal.PhaseSearch {
			t.Fatalf("the cancelled round's check ran: %+v", next)
		}
		assertProjectionReplay(h)
	})
	t.Run("a failure ends the round and the next round runs its own check", func(t *testing.T) {
		h, begin := roundCheckRetry(t)
		h.add(&journal.DeepeningRound{Round: 1, Event: journal.CycleEnd, Reason: "a failure needs a hunt"}, begin.Seq)
		nextRound(h)
		got := requireTrial(t, nextRoundCheck(t, h, 2))
		if got.Retry || got.Round != 2 {
			t.Fatalf("round 2 ran round 1's retry: %+v", got)
		}
		assertProjectionReplay(h)
	})
	t.Run("a search turn after the round ended does not run its check", func(t *testing.T) {
		h, begin := roundCheckRetry(t)
		h.add(&journal.DeepeningRound{Round: 1, Event: journal.CycleEnd, Reason: "a failure needs a hunt"}, begin.Seq)
		h.add(&journal.CorePhase{Core: 1, To: journal.PhaseSearch, Offset: -7, Reason: "test"})
		got := requireTrial(t, h.next())
		if got.Retry || got.Round != 0 || got.Core != 1 || got.Phase != journal.PhaseSearch {
			t.Fatalf("the ended round's check ran in the search turn: %+v", got)
		}
		assertProjectionReplay(h)
	})
}

// TestAllZeroRerunRetryBelongsToItsFailure retires the failure whose all-zero rerun ended inconclusively; the other
// failure's rerun runs as its own trial, loading its own cores and citing it.
func TestAllZeroRerunRetryBelongsToItsFailure(t *testing.T) {
	h := hasRoomHarness(t, 0, -10, 0, -10)
	w := machine.Workloads(machine.R5)[0].ID
	failureOf := map[int]int{}
	for _, core := range []int{0, 2} {
		tr := Trial{Core: core, Regime: machine.R5, Workload: w, Phase: journal.PhaseChecking, Condition: machine.Together, DurationS: 120}
		h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(core)})
		failureOf[core] = h.decide(h.next()).Seq
	}
	firstAction := h.next()
	first := requireTrial(t, firstAction)
	if !first.Rerun || first.Condition != machine.Parked || failureOf[first.Core] == 0 {
		t.Fatalf("first all-zero rerun %+v", first)
	}
	other := 0
	if first.Core == 0 {
		other = 2
	}
	h.trial(firstAction, unsure)
	if again := requireTrial(t, h.next()); !again.Retry || again.Core != first.Core {
		t.Fatalf("the rerun's own failure did not retry it: %+v", again)
	}

	h.add(&journal.CommandReset{Core: new(first.Core)})
	drainsNoTrial(t, h.s)
	var got Action
	for range 10 {
		got = h.next()
		if got.Kind == RunTrial {
			break
		}
		h.decide(got)
	}
	trial := requireTrial(t, got)
	want := ScheduledRequirement{Kind: "zero-rerun", Class: ScheduledClass{Regime: machine.R5, Workload: w, Cores: coresKey([]int{other}), DurationS: 120}, Since: failureOf[other], Rule: rerunEvidence, Needed: 1}
	if !trial.Rerun || trial.Retry || trial.Condition != machine.Parked || trial.Core != other || !slices.Equal(got.Cause, []int{failureOf[other]}) {
		t.Fatalf("the other failure's rerun is not its own trial: %+v", got)
	}
	if diff := cmp.Diff(want, trial.Requirement); diff != "" {
		t.Fatalf("rerun requirement (-want +got):\n%s", diff)
	}
	assertProjectionReplay(h)
}

// rerunObligations queues two rerun obligations of R1 120 s, cores 0 and 1, and returns their failures' sequences.
func rerunObligations(t *testing.T) (*harness, []int) {
	t.Helper()
	h := hasRoomHarness(t, -10, -11, -12, -13)
	var causes []int
	for core := range 2 {
		tr := Trial{Core: core, Regime: machine.R1, Workload: machine.Workloads(machine.R1)[1].ID, Condition: machine.Together, DurationS: 120}
		h.trial(Action{Kind: RunTrial, Trial: tr}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.ComputationError, Core: new(core)})
		failure := h.decide(h.next())
		causes = append(causes, failure.Seq)
		move := h.decide(h.next())
		h.add(&journal.ProfileChange{From: h.s.Profile(), To: h.s.offsets()}, move.Seq)
	}
	return h, causes
}

func TestRerunRetryBelongsToItsObligation(t *testing.T) {
	h, causes := rerunObligations(t)
	for range h.s.n - 1 {
		a, ok := h.s.rerunNext()
		if !ok || a.Trial.Core != 0 {
			t.Fatalf("first obligation's rerun %+v", a)
		}
		h.trial(a, passed)
	}
	a, ok := h.s.rerunNext()
	if !ok || a.Trial.Core != 0 {
		t.Fatalf("first obligation's last rerun %+v", a)
	}
	h.trial(a, unsure)
	again, ok := h.s.rerunNext()
	if !ok || !again.Trial.Retry || again.Trial.Core != 0 || !slices.Equal(again.Cause, []int{causes[0]}) {
		t.Fatalf("the obligation did not retry its own rerun: %+v", again)
	}

	// A resume that lowers n retires the first obligation, which its passes now answer.
	cfg := config.Default()
	cfg.Evidence.Miss = 0.1
	h.add(&journal.ConfigLoaded{Config: snapshotConfig(cfg)})
	if h.s.n >= config.Default().Evidence.Trials() {
		t.Fatalf("n = %d, did not drop", h.s.n)
	}
	drainsNoTrial(t, h.s)
	for name, a := range map[string]Action{"Next": h.next(), "rerunNext": func() Action { a, _ := h.s.rerunNext(); return a }()} {
		trial := requireTrial(t, a)
		if !trial.Rerun || trial.Retry || trial.Core != 1 || !slices.Equal(a.Cause, []int{causes[1]}) || trial.Requirement.Since != causes[1] {
			t.Fatalf("%s: the head obligation's rerun is not its own trial: %+v", name, a)
		}
	}
	assertProjectionReplay(h)
}

// TestResumedRequirementChangeDropsRetry resumes with a config that changes what a saved retry counted toward; the
// current requirement runs instead, and a resume that changes nothing keeps the retry.
func TestResumedRequirementChangeDropsRetry(t *testing.T) {
	t.Run("search duration", func(t *testing.T) {
		for _, tc := range []struct {
			name      string
			change    func(*config.Config)
			wantRetry bool
			duration  int
		}{
			{"unchanged", func(*config.Config) {}, true, 90},
			{"search_trial_s", func(c *config.Config) { c.Durations.SearchTrialS = 60 }, false, 60},
		} {
			t.Run(tc.name, func(t *testing.T) {
				cfg := config.Default()
				h := newHarness(t, searchAt(-10)...)
				h.trial(h.next(), unsure)
				tc.change(&cfg)
				h.add(&journal.ConfigLoaded{Config: snapshotConfig(cfg)})
				drainsNoTrial(t, h.s)
				got := requireTrial(t, h.next())
				if got.Retry != tc.wantRetry || got.DurationS != tc.duration || got.Regime != machine.R1 {
					t.Fatalf("after resume: %+v", got)
				}
				assertProjectionReplay(h)
			})
		}
	})
	t.Run("rerun short_trial_s", func(t *testing.T) {
		for _, tc := range []struct {
			name      string
			change    func(*config.Config)
			wantRetry bool
			duration  int
		}{
			{"unchanged", func(*config.Config) {}, true, 120},
			{"short_trial_s", func(c *config.Config) { c.Durations.ShortTrialS = 100 }, false, 100},
		} {
			t.Run(tc.name, func(t *testing.T) {
				cfg := config.Default()
				h, causes := rerunObligations(t)
				a, ok := h.s.rerunNext()
				if !ok {
					t.Fatal("no rerun")
				}
				h.trial(a, unsure)
				tc.change(&cfg)
				h.add(&journal.ConfigLoaded{Config: snapshotConfig(cfg)})
				a, ok = h.s.rerunNext()
				got := requireTrial(t, a)
				if !ok || got.Retry != tc.wantRetry || got.DurationS != tc.duration || got.Core != 0 || !slices.Equal(a.Cause, []int{causes[0]}) {
					t.Fatalf("after resume: %+v", a)
				}
				p := h.start(a).Data.(*journal.TrialIntent)
				if diff := cmp.Diff(got.Requirement, h.s.StoredRequirement(p.Trial)); diff != "" {
					t.Fatalf("attached requirement differs from the fold's (-attached +stored):\n%s", diff)
				}
			})
		}
	})
}

func TestCycleRetryEndsWithItsCycle(t *testing.T) {
	h := hasRoomHarness(t, -10, -20)
	steps := []machine.Regime{machine.R1}
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleStart, Steps: steps})
	h.trial(h.s.cycleNext(), unsure)
	if got := requireTrial(t, h.s.cycleNext()); !got.Retry || got.Cycle != 1 {
		t.Fatalf("the cycle's own step did not retry: %+v", got)
	}
	h.add(&journal.CheckingCycle{Cycle: 1, Event: journal.CycleEnd, Reason: "test"})
	h.add(&journal.CheckingCycle{Cycle: 2, Event: journal.CycleStart, Steps: steps})
	got := requireTrial(t, h.s.cycleNext())
	if got.Retry || got.Cycle != 2 {
		t.Fatalf("cycle 2 ran cycle 1's retry: %+v", got)
	}
	assertProjectionReplay(h)
}

func TestSearchRetryBelongsToItsTurn(t *testing.T) {
	h := newHarness(t, searchAt(-10, -10)...)
	a := requireTrial(t, h.next())
	h.trial(h.next(), unsure)
	if got := requireTrial(t, h.next()); !got.Retry || got.Core != a.Core || got.Regime != a.Regime || got.Offset != a.Offset {
		t.Fatalf("the search turn did not retry itself: %+v", got)
	}
	h.add(&journal.CommandReset{Core: new(1 - a.Core)})
	drainsNoTrial(t, h.s)
	for range 10 {
		next := h.next()
		if next.Kind == RunTrial {
			if next.Trial.Retry {
				t.Fatalf("a reset left the search retry: %+v", next)
			}
			break
		}
		h.decide(next)
	}
}

func TestHuntGroupRetryBelongsToItsGroup(t *testing.T) {
	h := huntHarness(t, 4, 120)
	var a Action
	for range 10 {
		a = h.next()
		if a.Kind == RunTrial {
			break
		}
		h.decide(a)
	}
	first := requireTrial(t, a)
	if first.Hunt == 0 {
		t.Fatalf("not a hunt trial: %+v", first)
	}
	h.trial(a, unsure)
	if got := requireTrial(t, h.next()); !got.Retry || got.Hunt != first.Hunt || got.Group != first.Group {
		t.Fatalf("the hunt group did not retry: %+v", got)
	}
	h.add(&journal.CommandReset{Core: new(0)})
	drainsNoTrial(t, h.s)
	for range 10 {
		next := h.next()
		if next.Kind == RunTrial {
			if next.Trial.Retry {
				t.Fatalf("a reset left the hunt retry: %+v", next)
			}
			break
		}
		h.decide(next)
	}
}

// TestHuntRetryStaysWithItsGroup gives two groups of one hunt the same requirement: both parts count evidence since the
// reset. Passes recorded under an older backend do not count until a resume restores it. Group 1 ends inconclusive and
// its retry is saved; the restore then establishes group 1 from those passes and the hunt plans group 2. Group 2 runs its
// own trial at its own profile, and group 1's retry is gone.
func TestHuntRetryStaysWithItsGroup(t *testing.T) {
	// session records the session up to its hunt start. Passes at prior, when set, precede the source failure, as an
	// earlier session's parked trials would, recorded under the old backend and followed by the new one.
	session := func(prior []int, duration int) *harness {
		t.Helper()
		starts := make([]coreStart, 4)
		for i := range starts {
			starts[i] = coreStart{phase: journal.PhaseAtLimit, offset: -30, fail: new(-31)}
		}
		h := newHarness(t, starts...)
		h.decide(h.next())
		ids, workload := h.s.ids(), machine.Workloads(machine.R6)[0].ID
		if prior != nil {
			loadBackends(h, mprimeOld, ycruncherOld)
			parked := Trial{Regime: machine.R6, Cores: ids, Workload: workload, Condition: machine.Parked, DurationS: duration, Profile: prior}
			for range h.s.n {
				h.trial(Action{Kind: RunTrial, Trial: parked}, passed)
			}
		}
		loadBackends(h, mprimeNew, ycruncherNew)
		source := Trial{Regime: machine.R6, Cores: ids, Workload: workload, Condition: machine.Together, Phase: journal.PhaseChecking, DurationS: 120, Profile: h.s.offsets()}
		h.trial(Action{Kind: RunTrial, Trial: source}, journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 5})
		h.decide(h.next())
		h.decide(driveToHuntStart(h))
		return h
	}
	plan := func(h *harness, number int) (*journal.HuntGroup, Action) {
		t.Helper()
		a := h.next()
		g, ok := a.Payload.(*journal.HuntGroup)
		if !ok {
			t.Fatalf("next %T %+v, want hunt group %d", a.Payload, a, number)
		}
		if g.Group != number || g.Stage != "part" && g.Stage != "complement" || g.Inferred != "" || g.Skipped {
			t.Fatalf("hunt group %+v, want group %d to run as a part or complement", *g, number)
		}
		return g, a
	}
	// A session without the passes plans the same group 1, which tells where to record them.
	g1, _ := plan(session(nil, 0), 1)
	p1 := slices.Clone(g1.Profile)

	h := session(p1, g1.DurationS)
	g1, a1 := plan(h, 1)
	if !slices.Equal(g1.Profile, p1) {
		t.Fatalf("group 1 plans %v with the old passes unrestored, want %v", g1.Profile, p1)
	}
	h.decide(a1)
	a := h.next()
	first := requireTrial(t, a)
	if first.Group != 1 || !slices.Equal(first.Profile, p1) {
		t.Fatalf("group 1 trial %+v", first)
	}
	h.trial(a, unsure)
	if got := requireTrial(t, h.next()); !got.Retry || got.Hunt != first.Hunt || got.Group != 1 || !slices.Equal(got.Profile, p1) {
		t.Fatalf("group 1 did not retry: %+v", got)
	}
	loadBackends(h, mprimeOld, ycruncherOld)
	g2, a2 := plan(h, 2)
	p2 := slices.Clone(g2.Profile)
	if slices.Equal(p1, p2) {
		t.Fatalf("groups 1 and 2 share profile %v", p1)
	}
	second := h.decide(a2)
	drainsNoTrial(t, h.s)
	want := h.next()
	got := requireTrial(t, want)
	if got.Retry || got.Hunt != first.Hunt || got.Group != 2 || !slices.Equal(got.Profile, p2) || !slices.Equal(want.Cause, []int{second.Seq}) {
		t.Fatalf("group 2 ran %+v, want its own trial at %v citing %d", want, p2, second.Seq)
	}
	if h.s.retry != nil {
		t.Fatalf("group 1's retry outlived its group: %+v", h.s.retry)
	}
	assertHuntNextReplay(h, want, (*State).Next, "replay changed group 2's trial")
	assertProjectionReplay(h)
}
