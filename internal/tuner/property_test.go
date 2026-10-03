package tuner

import (
	"math/rand/v2"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func TestSearchConverges(t *testing.T) {
	for seed := uint64(1); seed <= 120; seed++ {
		rng := rand.New(rand.NewPCG(seed, 0))
		threshold := -rng.IntN(50)
		h := newHarness(t, searchAt(-rng.IntN(51))...)
		settled := false
		for range 1000 {
			a := h.next()
			if a.Kind == Decide {
				e := h.decide(a)
				if p, ok := e.Data.(*journal.CorePhase); ok && p.To != journal.PhaseSearch {
					if p.Offset != threshold {
						t.Fatalf("seed %d: solo limit %d, want %d", seed, p.Offset, threshold)
					}
					settled = true
					break
				}
				continue
			}
			if a.Trial.Offset >= threshold {
				h.trial(a, passed)
			} else {
				h.trial(a, failed)
			}
		}
		if !settled {
			t.Fatalf("seed %d did not find solo limit", seed)
		}
	}
}

func TestRandomOutcomesPreserveProfiles(t *testing.T) {
	parkedFailures := 0
	for seed := uint64(1); seed <= 12; seed++ {
		rng := rand.New(rand.NewPCG(seed, 1))
		h := hasRoomHarness(t, -30, -30, -30, -30)
		started, ended := 0, 0
		for step := range 700 {
			if step >= 350 && h.s.hunt == nil && started > 0 {
				break
			}
			a := h.next()
			if a.Kind == Decide {
				if _, ok := a.Payload.(*journal.DeadEnd); ok {
					break
				}
				if p, ok := a.Payload.(*journal.ProfileChange); ok {
					if name, reached := h.s.Reaches(p.To); reached {
						t.Fatalf("seed %d: profile change reached %s", seed, name)
					}
				}
				combinations := append([]journal.CombinationState(nil), h.s.combinations...)
				e := h.decide(a)
				if _, ok := e.Data.(*journal.HuntStart); ok {
					started++
				}
				if _, ok := e.Data.(*journal.HuntEnd); ok {
					ended++
				}
				for _, combination := range combinations {
					still := false
					for _, current := range h.s.combinations {
						if current.Combination == combination.Combination && cmp.Diff(combination.Members, current.Members) == "" {
							still = true
						}
					}
					if !still {
						t.Fatalf("seed %d: combination C%d disappeared without reset", seed, combination.Combination)
					}
				}
				continue
			}
			intent := h.start(a)
			p := intent.Data.(*journal.TrialIntent)
			if name, reached := h.s.Reaches(p.Profile); reached {
				t.Fatalf("seed %d: trial %s reached %s", seed, p.Trial, name)
			}
			for _, offset := range p.Profile {
				if offset < machine.MinOffset || offset > machine.MaxOffset {
					t.Fatalf("seed %d: offset %d outside range", seed, offset)
				}
			}
			if p.Condition == machine.Alone {
				for i, x := range p.Profile {
					if i != *p.Core && x != 0 {
						t.Fatalf("seed %d: alone profile %v", seed, p.Profile)
					}
				}
			}
			end := passed
			if rng.Float64() < .035 {
				end = journal.TrialEnd{Outcome: journal.OutcomeFailure, Signal: machine.Crash, DurationS: 10}
				if p.Condition == machine.Parked {
					parkedFailures++
				}
			} else if rng.Float64() < .06 {
				end = unsure
			}
			end.Trial = p.Trial
			h.add(&end, intent.Seq)
		}
		if started == 0 || ended == 0 || started != ended {
			t.Fatalf("seed %d: hunts started %d, ended %d", seed, started, ended)
		}
	}
	if parkedFailures == 0 {
		t.Fatal("fixed seeds did not exercise a failed parked trial")
	}
}
