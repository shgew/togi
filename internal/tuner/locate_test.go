package tuner

import (
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
)

func isFullGroup(duration int) func(Action) bool {
	return func(a Action) bool {
		g, ok := a.Payload.(*journal.HuntGroup)
		return ok && g.Stage == "full" && g.DurationS == duration
	}
}

func assertFullFailingGroup(h *harness, a Action, duration int) {
	h.t.Helper()
	g, ok := a.Payload.(*journal.HuntGroup)
	if !ok || g.Stage != "full" || g.DurationS != duration || !slices.Equal(g.Cores, []int{2, 3}) || !slices.Equal(g.Profile, []int{-30, -30, -30, -30}) {
		h.t.Fatalf("full failing profile group %+v, want cores [2 3] at %d s", a, duration)
	}
}

// assertLoadedCoresKept answers the decisions that follow a hunt end and fails if one moves a loaded core.
func assertLoadedCoresKept(h *harness) {
	h.t.Helper()
	for a := h.next(); a.Kind == Decide; a = h.next() {
		if move, ok := a.Payload.(*journal.TunerDecision); ok && (move.Core == 0 || move.Core == 1) {
			h.t.Fatalf("a loaded core moved: %+v", move)
		}
		h.decide(a)
	}
	if got := h.s.offsets()[:2]; !slices.Equal(got, []int{-30, -30}) {
		h.t.Fatalf("loaded offsets %v, want [-30 -30]", got)
	}
}

func jointFailure(duration int) func(Trial) *int {
	return func(t Trial) *int { return unnamed(t.DurationS == duration && t.Profile[2] != 0 && t.Profile[3] != 0) }
}

func never(Trial) *int { return nil }

func TestLocatedHuntRetestsTheFullFailingProfileBeforeEndingLoaded(t *testing.T) {
	t.Run("full profile fails", func(t *testing.T) {
		h := r7Harness(t)
		failEscalatedR7(h, journal.TrialEnd{DurationS: 41, TopRequesters: []int{0}})
		a := runLocatedUntil(h, jointFailure(120), isFullGroup(120))
		assertFullFailingGroup(h, a, 120)
		if n := len(h.s.hunt.groups); n != 3 {
			t.Fatalf("%d groups before the full group, want locate and both singletons", n)
		}
		a = runLocatedUntil(h, jointFailure(120), func(Action) bool { return false })
		end, ok := a.Payload.(*journal.HuntEnd)
		if !ok || end.Result != "combination" || !slices.Equal(end.Cores, []int{2, 3}) {
			t.Fatalf("hunt end %+v, want combination [2 3]", a)
		}
		h.decide(a)
		assertLoadedCoresKept(h)
		assertProjectionReplay(h)
	})
	t.Run("full profile passes", func(t *testing.T) {
		h := r7Harness(t)
		failEscalatedR7(h, journal.TrialEnd{DurationS: 41, TopRequesters: []int{0}})
		a := runLocatedUntil(h, never, isFullGroup(120))
		assertFullFailingGroup(h, a, 120)
		h.decide(a)
		h.trial(h.next(), passed)
		want := h.s.Next()
		if want.Kind != RunTrial || want.Trial.Group != 4 || want.Trial.DurationS != 120 || !slices.Equal(want.Trial.Profile, []int{-30, -30, -30, -30}) {
			t.Fatalf("full group did not continue: %+v", want)
		}
		assertHuntNextReplay(h, want, (*State).Next, "replay changed the full group trial")
		if got := projected(h).Hunt.Groups[3]; got.Passes != 1 || got.Outcome != "running" {
			t.Fatalf("full group progress after resume: %+v", got)
		}
		a = runLocated(h, func([]int) *int { return nil })
		end, ok := a.Payload.(*journal.HuntEnd)
		if !ok || end.Result != "loaded" || !slices.Equal(end.Cores, []int{0, 1}) || end.Groups != 4 {
			t.Fatalf("hunt end %+v, want loaded after 4 groups", a)
		}
		ended := h.decide(a)
		a = h.next()
		move, ok := a.Payload.(*journal.TunerDecision)
		if !ok || move.Decision != journal.Backoff || move.Core != 0 || move.ToOffset != -29 || !slices.Contains(a.Cause, ended.Seq) {
			t.Fatalf("loaded backoff %+v, want core 00 to -29 citing hunt end #%d", a, ended.Seq)
		}
		assertProjectionReplay(h)
	})
}

func TestEscalatedLocatedHuntRetestsTheFullFailingProfileAtTheFailedDuration(t *testing.T) {
	for _, tc := range []struct {
		name   string
		fails  func(Trial) *int
		result string
		cores  []int
	}{
		{"full profile passes", never, "loaded", []int{0, 1}},
		{"full profile fails", jointFailure(120), "combination", []int{2, 3}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := r7Harness(t)
			c := config.Default()
			c.Durations.ShortTrialS = 60
			h.add(&journal.ConfigLoaded{Path: config.DefaultPath, Config: snapshotConfig(c)})
			failEscalatedR7(h, journal.TrialEnd{DurationS: 41, TopRequesters: []int{0}})
			a := runLocatedUntil(h, tc.fails, isFullGroup(120))
			assertFullFailingGroup(h, a, 120)
			var stages []string
			for _, m := range h.s.hunt.groups {
				stages = append(stages, m.payload.Stage)
				if m.payload.Stage == "full" && m.payload.DurationS != 60 {
					t.Fatalf("short full group at %d s", m.payload.DurationS)
				}
			}
			if diff := cmp.Diff([]string{"locate", "part", "part", "full", "part", "part"}, stages); diff != "" {
				t.Fatalf("groups before the full group at the failed duration (-want +got):\n%s", diff)
			}
			h.decide(a)
			a = runLocatedUntil(h, tc.fails, func(Action) bool { return false })
			end, ok := a.Payload.(*journal.HuntEnd)
			if !ok || end.Result != tc.result || !slices.Equal(end.Cores, tc.cores) {
				t.Fatalf("hunt end %+v, want %s %v", a, tc.result, tc.cores)
			}
			h.decide(a)
			if tc.result != "loaded" {
				assertLoadedCoresKept(h)
			}
			assertProjectionReplay(h)
		})
	}
}

func TestResetOfACandidateCancelsTheLocatedHunt(t *testing.T) {
	h := r7Harness(t)
	_, failure := failEscalatedR7(h, journal.TrialEnd{DurationS: 41, TopRequesters: []int{0}})
	for range 2 {
		h.decide(h.next())
	}
	h.trial(h.next(), passed)
	h.add(&journal.CommandReset{Core: new(2)})
	a := h.next()
	if end, ok := a.Payload.(*journal.HuntEnd); !ok || end.Result != "cancelled" || end.Hunt != 1 {
		t.Fatalf("reset did not cancel the located hunt: %+v", a)
	}
	h.decide(a)
	if _, ok := h.s.located[failure.Seq]; ok || h.s.hunt != nil {
		t.Fatalf("cancelled located hunt remains: hunt %+v, located %+v", h.s.hunt, h.s.located)
	}
	a = h.next()
	if phase, ok := a.Payload.(*journal.CorePhase); !ok || phase.Core != 2 || phase.To != journal.PhaseSearch {
		t.Fatalf("reset %+v", a)
	}
	h.decide(a)
	if f, due := h.s.locateDue(); !due || f.seq != failure.Seq {
		t.Fatalf("failure #%d no longer waits for a located hunt", failure.Seq)
	}
	assertHuntNextReplay(h, h.s.Next(), (*State).Next, "replay changed the action after the cancelled located hunt")
	assertProjectionReplay(h)
	h.add(&journal.CorePhase{Core: 2, From: journal.PhaseSearch, To: journal.PhaseHasRoom, Offset: -30, Reason: "test"})
	for range 10 {
		a = h.next()
		if start, ok := a.Payload.(*journal.HuntStart); ok {
			if start.Hunt != 2 || start.Failure != failure.Seq || !slices.Equal(start.Candidates, []int{2, 3}) {
				t.Fatalf("relocated hunt %+v", start)
			}
			return
		}
		if move, ok := a.Payload.(*journal.TunerDecision); ok {
			t.Fatalf("the cancelled located failure moved a core: %+v", move)
		}
		if a.Kind != Decide {
			t.Fatalf("failure #%d was not located again: %+v", failure.Seq, a)
		}
		h.decide(a)
	}
	t.Fatal("failure was not located again")
}
