package tuner

import (
	"fmt"
	"math"
	"slices"

	"github.com/shgew/togi/internal/machine"
)

func (s *State) reaches(p []int) (string, bool) {
	limit, id, offset := s.reachedLimit(p)
	return limitReason(limit, id, offset)
}

func limitReason(limit Limit, id, offset int) (string, bool) {
	switch {
	case limit.Floor:
		return "at the floor -50", true
	case limit.Failure:
		return fmt.Sprintf("failure point %d of core %02d", offset, id), true
	case limit.Combination != 0:
		return fmt.Sprintf("combination C%d", limit.Combination), true
	default:
		return "", false
	}
}

func (s *State) reachedLimit(p []int) (Limit, int, int) {
	if len(p) != len(s.cores) {
		return Limit{}, 0, 0
	}
	for _, c := range s.byID() {
		if c.fail != nil && p[s.index(c.id)] <= *c.fail {
			return Limit{Failure: true}, c.id, *c.fail
		}
	}
	for _, m := range s.combinations {
		all := true
		for _, member := range m.Members {
			if p[s.index(member.Core)] > member.Offset {
				all = false
				break
			}
		}
		if all {
			return Limit{Combination: m.Combination}, 0, 0
		}
	}
	return Limit{}, 0, 0
}

func (s *State) Reaches(profile []int) (string, bool) { return s.reaches(profile) }
func SoleNonzero(profile []int) (index int, ok bool) {
	for i, v := range profile {
		if v != 0 {
			if ok {
				return 0, false
			}
			index, ok = i, true
		}
	}
	return
}

func (s *State) atLimit(c *core, p []int) (string, bool) {
	limit, id, offset := s.limitAt(c, p)
	return limitReason(limit, id, offset)
}

func (s *State) limitAt(c *core, p []int) (Limit, int, int) {
	if p[s.index(c.id)] == machine.MinOffset {
		return Limit{Floor: true}, 0, 0
	}
	q := slices.Clone(p)
	q[s.index(c.id)]--
	return s.reachedLimit(q)
}

// optimum returns the deepest-total profile within [floor, hi] that reaches no
// combination, breaking ties by ranking then core-id order, or nil if none exists.
// Every core ends at its floor or at a count that breaks some combination, so the
// search branches only over which member breaks each combination still reached.
func (s *State) optimum(hi, ranking []int) []int {
	n := len(s.cores)
	p := make([]int, n)
	total := 0
	for i, c := range s.byID() {
		p[i] = machine.MinOffset
		if c.fail != nil {
			p[i] = max(machine.MinOffset, *c.fail+1)
		}
		if p[i] > hi[i] {
			return nil
		}
		total += p[i]
	}
	needs, ok := s.breakThresholds(p, hi)
	if !ok {
		return nil
	}
	order := s.tieOrder(ranking)
	var best []int
	bestTotal := math.MaxInt
	moves := make([][]move, len(needs)+1)
	var visit func(depth int)
	visit = func(depth int) {
		branch, branchMoves, bound := -1, 0, 0
		for k, need := range needs {
			cheapest, count := math.MaxInt, 0
			for i, at := range need {
				if at == unbreakable {
					continue
				}
				if p[i] >= at {
					count = 0
					break
				}
				cheapest = min(cheapest, at-p[i])
				count++
			}
			if count == 0 {
				continue
			}
			bound = max(bound, cheapest)
			if branch < 0 || count < branchMoves {
				branch, branchMoves = k, count
			}
		}
		if total+bound > bestTotal {
			return
		}
		if branch < 0 {
			if total < bestTotal || deeperOnTie(p, best, order) {
				best, bestTotal = slices.Clone(p), total
			}
			return
		}
		options := moves[depth][:0]
		for _, i := range order {
			if at := needs[branch][i]; at != unbreakable {
				options = append(options, move{i, at})
			}
		}
		slices.SortStableFunc(options, func(a, b move) int { return (a.at - p[a.core]) - (b.at - p[b.core]) })
		moves[depth] = options
		for _, m := range options {
			prior := p[m.core]
			p[m.core] = m.at
			total += m.at - prior
			visit(depth + 1)
			total -= m.at - prior
			p[m.core] = prior
		}
	}
	visit(0)
	return best
}

const unbreakable = math.MaxInt

type move struct{ core, at int }

// breakThresholds lists, for each combination the floor profile p reaches, the
// offset at or above which each core breaks it, or unbreakable when that offset
// exceeds hi. It drops each combination another one already implies, and reports
// false when some combination cannot be broken within hi.
func (s *State) breakThresholds(p, hi []int) ([][]int, bool) {
	var needs [][]int
	for _, m := range s.combinations {
		need := make([]int, len(p))
		for i := range need {
			need[i] = unbreakable
		}
		reached, breakable := true, false
		for _, v := range m.Members {
			i := s.index(v.Core)
			if p[i] > v.Offset {
				reached = false
				break
			}
			if v.Offset+1 <= hi[i] {
				need[i] = v.Offset + 1
				breakable = true
			}
		}
		if !reached {
			continue
		}
		if !breakable {
			return nil, false
		}
		needs = append(needs, need)
	}
	implied := func(a, b []int) bool {
		for i, at := range a {
			if at != unbreakable && b[i] > at {
				return false
			}
		}
		return true
	}
	kept := make([][]int, 0, len(needs))
	for k, b := range needs {
		redundant := false
		for j, a := range needs {
			if j != k && implied(a, b) && (j < k || !implied(b, a)) {
				redundant = true
				break
			}
		}
		if !redundant {
			kept = append(kept, b)
		}
	}
	return kept, true
}

func (s *State) tieOrder(ranking []int) []int {
	if len(ranking) != len(s.cores) {
		ranking = s.ids()
	}
	order := make([]int, 0, len(s.cores))
	placed := make([]bool, len(s.cores))
	for _, id := range ranking {
		if i := s.index(id); i >= 0 && !placed[i] {
			order = append(order, i)
			placed[i] = true
		}
	}
	for i := range placed {
		if !placed[i] {
			order = append(order, i)
		}
	}
	return order
}

func deeperOnTie(p, best, order []int) bool {
	for _, i := range order {
		if p[i] != best[i] {
			return p[i] < best[i]
		}
	}
	return false
}
