package tuner

import (
	"math/rand/v2"
	"testing"

	"code.marleb.org/shgew/shycler/internal/journal"
	"code.marleb.org/shgew/shycler/internal/machine"
)

const (
	propertySeeds = 500
	maxActions    = 10000
	propertyCores = 4
)

type stop struct {
	dead  *journal.DeadEnd
	guard bool
}

func drive(h *harness, outcome func(Trial) journal.TrialEnd, check func(Action)) stop {
	for range maxActions {
		a := h.s.Next()
		if check != nil {
			check(a)
		}
		switch a.Kind {
		case Decide:
			h.decide(a)
			switch p := a.Payload.(type) {
			case *journal.DeadEnd:
				return stop{dead: p}
			case *journal.ProfileChange:
				return stop{guard: true}
			}
		case RunTrial:
			h.trial(a, outcome(a.Trial))
		}
	}
	h.t.Fatalf("no dead end or guard after %d actions", maxActions)
	return stop{}
}

func TestConvergesOnDeepestPassingOffset(t *testing.T) {
	t.Parallel()
	for seed := uint64(1); seed <= propertySeeds; seed++ {
		rng := rand.New(rand.NewPCG(seed, 0))
		starts := make([]int, propertyCores)
		thresholds := make([]map[machine.Regime]int, propertyCores)
		edge := make([]int, propertyCores)
		for c := range starts {
			starts[c] = -rng.IntN(51)
			thresholds[c] = map[machine.Regime]int{}
			edge[c] = -51
			for _, r := range machine.ConfirmationRegimes {
				thresholds[c][r] = 1 - rng.IntN(53)
				edge[c] = max(edge[c], thresholds[c][r])
			}
		}
		h := newHarness(t, searchAt(starts...)...)
		got := drive(h, func(tr Trial) journal.TrialEnd {
			if tr.Offset >= thresholds[tr.Core][tr.Regime] {
				return passed
			}
			return failed
		}, nil)

		unstable := false
		for c := range edge {
			unstable = unstable || edge[c] > 0
		}
		if unstable {
			if got.dead == nil || got.dead.Condition != journal.DeadEndFailureAtZero || edge[*got.dead.Core] <= 0 {
				t.Fatalf("seed %d: edges %v: stopped with %+v, want failure_at_zero on a core unstable at 0", seed, edge, got.dead)
			}
			continue
		}
		if !got.guard {
			t.Fatalf("seed %d: edges %v: stopped with %+v, want guard", seed, edge, got.dead)
		}
		st := projected(h)
		for c, cs := range st.Cores {
			if want := max(edge[c], machine.MinOffset); cs.Phase != journal.PhaseConfirmed || cs.Offset != want {
				t.Fatalf("seed %d: core %d %s at %d, want confirmed at %d", seed, c, cs.Phase, cs.Offset, want)
			}
		}
	}
}

func TestInvariantsOverRandomOutcomes(t *testing.T) {
	t.Parallel()
	for seed := uint64(1); seed <= propertySeeds; seed++ {
		rng := rand.New(rand.NewPCG(seed, 1))
		starts := make([]int, propertyCores)
		for c := range starts {
			starts[c] = -rng.IntN(51)
		}
		h := newHarness(t, searchAt(starts...)...)
		failedMark := map[int]int{}
		inRange := func(what string, o int) {
			if o < machine.MinOffset || o > machine.MaxOffset {
				t.Fatalf("seed %d: %s offset %d outside [-50, 0]", seed, what, o)
			}
		}
		drive(h, func(Trial) journal.TrialEnd {
			switch x := rng.Float64(); {
			case x < 0.5:
				return passed
			case x < 0.9:
				return failed
			}
			return unsure
		}, func(a Action) {
			switch a.Kind {
			case RunTrial:
				inRange("trial", a.Trial.Offset)
				if f, ok := failedMark[a.Trial.Core]; ok && a.Trial.Offset <= f {
					t.Fatalf("seed %d: trial on core %d at %d, at or deeper than its failed mark %d", seed, a.Trial.Core, a.Trial.Offset, f)
				}
			case Decide:
				switch p := a.Payload.(type) {
				case *journal.Failure:
					if f, ok := failedMark[*p.Core]; !ok || *p.Offset > f {
						failedMark[*p.Core] = *p.Offset
					}
				case *journal.TunerDecision:
					inRange("decision", p.ToOffset)
				case *journal.CorePhase:
					inRange("phase", p.Offset)
				}
			}
		})
	}
}
