package tuner

import (
	"fmt"
	"slices"

	"github.com/shgew/togi/internal/journal"
	"github.com/shgew/togi/internal/machine"
)

func (s *State) reaches(p []int) (string, bool) {
	if len(p) != len(s.cores) {
		return "", false
	}
	for _, c := range s.byID() {
		if c.fail != nil && p[s.index(c.id)] <= *c.fail {
			return fmt.Sprintf("failed mark %d of core %02d", *c.fail, c.id), true
		}
	}
	for _, m := range s.marks {
		all := true
		for _, member := range m.Members {
			if p[s.index(member.Core)] > member.Offset {
				all = false
				break
			}
		}
		if all {
			return fmt.Sprintf("joint mark J%d", m.Mark), true
		}
	}
	return "", false
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

func (s *State) done(c *core, p []int) (string, bool) {
	if c.offset == machine.MinOffset {
		return "at the floor -50", true
	}
	q := slices.Clone(p)
	q[s.index(c.id)]--
	return s.reaches(q)
}

func (s *State) optimum(hi, ranking []int) []int {
	n := len(s.cores)
	lo := make([]int, n)
	for i, c := range s.byID() {
		lo[i] = machine.MinOffset
		if c.fail != nil {
			lo[i] = max(machine.MinOffset, *c.fail+1)
		}
	}
	active := make([]journal.JointMarkState, 0, len(s.marks))
	for _, m := range s.marks {
		reached := true
		for _, v := range m.Members {
			if lo[s.index(v.Core)] > v.Offset {
				reached = false
				break
			}
		}
		if reached {
			active = append(active, m)
		}
	}
	slices.SortFunc(active, func(a, b journal.JointMarkState) int { return a.Mark - b.Mark })
	if len(ranking) != n {
		ranking = s.ids()
	}
	best := []int(nil)
	bestSum := int(^uint(0) >> 1)
	caps := make([]int, n)
	sum := func(cap []int) int {
		total := 0
		for i := range lo {
			total += max(lo[i], cap[i])
		}
		return total
	}
	var visit func(int)
	for i := range caps {
		caps[i] = machine.MinOffset
	}
	visit = func(at int) {
		if sum(caps) > bestSum {
			return
		}
		if at == len(active) {
			candidate := make([]int, n)
			for i := range candidate {
				candidate[i] = max(lo[i], caps[i])
				if candidate[i] > hi[i] {
					return
				}
			}
			total := sum(caps)
			better := total < bestSum
			if total == bestSum {
				for _, id := range ranking {
					i := s.index(id)
					if i < 0 {
						continue
					}
					if candidate[i] != best[i] {
						better = candidate[i] < best[i]
						break
					}
				}
			}
			if better {
				bestSum = total
				best = candidate
			}
			return
		}
		mark := active[at]
		for _, m := range mark.Members {
			if i := s.index(m.Core); i >= 0 && max(lo[i], caps[i]) > m.Offset {
				visit(at + 1)
				return
			}
		}
		members := slices.Clone(mark.Members)
		slices.SortFunc(members, func(a, b journal.JointMember) int {
			ia, ib := s.index(a.Core), s.index(b.Core)
			lossA := max(lo[ia], a.Offset+1) - lo[ia]
			lossB := max(lo[ib], b.Offset+1) - lo[ib]
			if lossA != lossB {
				return lossA - lossB
			}
			return a.Core - b.Core
		})
		for _, m := range members {
			i := s.index(m.Core)
			if i < 0 || m.Offset+1 > hi[i] {
				continue
			}
			prior := caps[i]
			caps[i] = max(caps[i], m.Offset+1)
			visit(at + 1)
			caps[i] = prior
		}
	}
	visit(0)
	return best
}

func (s *State) best() []int {
	if s.bestDirty {
		s.bestProfile = s.optimum(make([]int, len(s.cores)), s.ranking)
		s.bestDirty = false
	}
	return s.bestProfile
}
