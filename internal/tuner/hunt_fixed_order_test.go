package tuner

import (
	"fmt"
	"slices"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

// huntPartOrder is the partition order the hunt fixes when its hunt.start is folded, with recent [3] on four
// candidates: the part holding core 03 first, the others in candidate order.
func huntPartOrder(stage string, granularity int) [][]int {
	switch {
	case granularity == 2:
		return [][]int{{2, 3}, {0, 1}}
	case stage == "part":
		return [][]int{{3}, {0}, {1}, {2}}
	default:
		return [][]int{{0, 1, 2}, {1, 2, 3}, {0, 2, 3}, {0, 1, 3}}
	}
}

func assertFixedGroup(t *testing.T, where string, g *journal.HuntGroup) {
	t.Helper()
	if g.Stage != "part" && g.Stage != "complement" {
		return
	}
	order := huntPartOrder(g.Stage, g.Granularity)
	if g.Index >= len(order) || !slices.Equal(g.Cores, order[g.Index]) {
		t.Fatalf("%s: group %d (%s, granularity %d, index %d) plans cores %v, want the part fixed at hunt.start: %v", where, g.Group, g.Stage, g.Granularity, g.Index, g.Cores, order)
	}
}

// assertFixedPlan checks what one reducer plans next and what its dashboard projects for the open hunt.
func assertFixedPlan(t *testing.T, where string, s *State) {
	t.Helper()
	if g, ok := s.Next().Payload.(*journal.HuntGroup); ok {
		assertFixedGroup(t, where+" Next", g)
	}
	plan := s.HuntPlan()
	if plan == nil || len(plan.Parts) < 2 {
		return
	}
	var failing [][]int
	for _, p := range plan.Parts {
		failing = append(failing, p.Failing)
		if p.Group == 0 {
			continue
		}
		i := slices.IndexFunc(plan.Groups, func(g HuntGroup) bool { return g.Number == p.Group })
		if i < 0 || !slices.Equal(plan.Groups[i].Cores, p.Failing) {
			t.Fatalf("%s: dashboard part %v claims group %d, which ran other cores: %+v", where, p.Failing, p.Group, plan.Groups)
		}
	}
	for _, want := range [][][]int{huntPartOrder("part", 2), huntPartOrder("part", 4), huntPartOrder("complement", 4)} {
		if cmp.Diff(want, failing) == "" {
			return
		}
	}
	t.Fatalf("%s: dashboard parts %v are not the order fixed at hunt.start", where, failing)
}

// TestHuntKeepsOnePartOrderWhenRecentMovesMidHunt moves the live recent cores while a hunt is open, through the
// journal facts a Ruleset 10 session recorded for a crash while applying a hunt group's parked profile: an attributed
// together failure with no trial, naming the sole nonzero core. The tuner backs that core off, which moves recent
// to it, and the hunt stays open. Every part, then every complement, must still be planned once in the order fixed
// when the hunt started, in Next, on the dashboard, and after replaying every prefix a resumed session could see.
func TestHuntKeepsOnePartOrderWhenRecentMovesMidHunt(t *testing.T) {
	// part index of granularity 4 whose application crashes; its sole core becomes the live recent core.
	for _, crashAt := range []int{1, 2, 3} {
		t.Run(fmt.Sprintf("crash during part %d", crashAt), func(t *testing.T) {
			h := huntHarness(t, 4, 120)
			if !slices.Equal(h.s.recent, []int{3}) {
				t.Fatalf("hunt starts with recent %v, want [3]", h.s.recent)
			}
			crashed := false
			rebooted := 0 // index of the first event recorded by the boot after the crash
			planned := map[int]*journal.HuntGroup{}
			var order []int
			for range 400 {
				if crashed {
					for i := rebooted; i < len(h.events); i++ {
						h.events[i].Boot = "b2"
					}
				}
				prefix := len(h.events)
				assertHuntNextReplay(h, h.s.Next(), (*State).Next, "resume at event prefix (-live +replay)")
				assertFixedPlan(t, "live", h.s)
				assertFixedPlan(t, "replay", replayState(h.events[:prefix]))
				a := h.next()
				switch p := a.Payload.(type) {
				case *journal.HuntGroup:
					assertFixedGroup(t, "planned", p)
					if _, seen := planned[p.Group]; !seen {
						order = append(order, p.Group)
					}
					planned[p.Group] = p
					h.decide(a)
					if !crashed && p.Stage == "part" && p.Granularity == 4 && p.Index == crashAt && !p.Skipped {
						crashed = true
						core := p.Cores[0]
						// The application is interrupted after its first nonzero write: no profile.applied, no trial.intent.
						intent := h.add(&journal.SMUIntent{Op: journal.SMUSet, Core: new(core), Offset: -30}, h.s.ProfileSeq())
						write := h.add(&journal.SMUWrite{Op: journal.SMUSet, Core: new(core), Offset: -30}, intent.Seq)
						readback := h.add(&journal.SMUReadback{Core: core, Offset: -30, Expected: new(-30)}, write.Seq)
						rebooted = len(h.events)
						crash := h.add(&journal.CrashDetected{PreviousBoot: "b", Condition: machine.Together}, readback.Seq)
						profile := make([]int, 4)
						profile[core] = -30
						h.add(&journal.Failure{Signal: machine.Crash, Attribution: journal.Attributed, Core: new(core), Offset: new(-30), Regime: machine.R6, Condition: machine.Together, Profile: profile}, crash.Seq)
						back := h.next()
						d, ok := back.Payload.(*journal.TunerDecision)
						if !ok || d.Decision != journal.Backoff || d.Core != core {
							t.Fatalf("attributed application failure decided %+v, want a backoff of core %d", back, core)
						}
						h.decide(back)
						if h.s.hunt == nil || h.s.hunt.end != nil || !slices.Equal(h.s.recent, []int{core}) {
							t.Fatalf("hunt %+v recent %v after the backoff, want the hunt open and recent [%d]", h.s.hunt, h.s.recent, core)
						}
					}
				case *journal.HuntEnd:
					if !crashed {
						t.Fatalf("hunt ended before the crash: %+v", p)
					}
					var got [][]int
					for _, n := range order {
						if g := planned[n]; g.Granularity == 4 {
							got = append(got, g.Cores)
						}
					}
					want := append(huntPartOrder("part", 4), huntPartOrder("complement", 4)...)
					if diff := cmp.Diff(want, got); diff != "" {
						t.Fatalf("granularity 4 groups, each planned once in the fixed order (-want +got):\n%s", diff)
					}
					return
				default:
					if a.Kind == Decide {
						h.decide(a)
						continue
					}
					runGroup(h, a, false)
				}
			}
			t.Fatal("hunt never ended")
		})
	}
}
